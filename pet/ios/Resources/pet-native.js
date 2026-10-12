/* Generated from pet/src/core. Run npm --prefix pet run sync. */
(function(global){
'use strict';
if(!global.structuredClone)global.structuredClone=value=>JSON.parse(JSON.stringify(value));
const factories={},cache={};
factories["pet-native"]=function(exports,require){
"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.PetNative = void 0;
const pet_world_js_1 = require("./pet-world.js");
const pet_telemetry_js_1 = require("./pet-telemetry.js");
const pet_sim_js_1 = require("./pet-sim.js");
const pet_audio_js_1 = require("./pet-audio.js");
const pet_engine_js_1 = require("./pet-engine.js");
const dot_field_js_1 = require("./dot-field.js");
const dot_glyphs_js_1 = require("./dot-glyphs.js");
/** Synchronous native boundary: JSON state and directly transferable PCM.
 * Native hosts share the actual world / score implementation, not a rewrite. */
class PetNative {
    world;
    engine = new pet_engine_js_1.PetEngineTelemetry();
    expressionActivity = this.engine.activity(0);
    expressionTransition;
    expressionOptions = { bodyStyle: 'smooth', textLanguage: 'en' };
    engineTick = 0;
    segment;
    liveTape = new pet_telemetry_js_1.PetLiveTape();
    stillProjection;
    constructor(pointsJSON, tapeJSONL = '', interactionsJSON = '[]', live = false, expressionVersion = 2) {
        const points = JSON.parse(pointsJSON);
        if (!Array.isArray(points) || points.length !== 980 || points.some(p => !Array.isArray(p) || p.length !== 2 || !p.every(n => Number.isFinite(n) && Math.abs(n) <= 1)))
            throw new Error('Invalid native whale body.');
        this.world = new pet_world_js_1.PetWorld(points, live ? (0, pet_telemetry_js_1.compilePetTelemetry)([]) : (0, pet_telemetry_js_1.decodePetJSONL)(tapeJSONL), JSON.parse(interactionsJSON), expressionVersion, true);
    }
    step(dt, motion) { this.world.step(dt, { motion, sensitivity: 1 }); return this.snapshot(); }
    snapshot() { return JSON.stringify({ ...this.world.frame, voices: this.world.voices, digest: (0, pet_sim_js_1.digest)(this.world.sim) }); }
    expression() {
        const { frame } = this.world;
        const target = (0, dot_field_js_1.projectDotField)(this.expressionActivity, frame.timeMs, false, this.expressionTransition?.at ?? 0, this.expressionOptions);
        return this.expressionTransition
            ? (0, dot_field_js_1.blendDotFields)(this.expressionTransition.from, target, (frame.timeMs - this.expressionTransition.at) / 560)
            : target;
    }
    prepareExpression(activity) {
        if ((0, dot_field_js_1.dotFieldKey)(activity) !== (0, dot_field_js_1.dotFieldKey)(this.expressionActivity))
            this.expressionTransition = { at: this.world.frame.timeMs, from: this.expression() };
        this.expressionActivity = activity;
    }
    /** View-only projection. Display cadence and accessibility preferences never
     * advance the owner, consume randomness, or change its score/checkpoint. */
    presentation(options = {}, interfaceLanguage = 'en') {
        if (!['smooth', 'pixel'].includes(options.bodyStyle ?? 'smooth')
            || !['auto', 'en', 'zh-Hans', 'zh-Hant', 'ja'].includes(options.textLanguage ?? 'auto'))
            throw new Error('Invalid pet presentation preferences.');
        const next = { bodyStyle: options.bodyStyle ?? 'smooth', textLanguage: (0, dot_field_js_1.textLanguage)(options.textLanguage, interfaceLanguage) };
        if (next.bodyStyle !== this.expressionOptions.bodyStyle || next.textLanguage !== this.expressionOptions.textLanguage) {
            const from = this.expression();
            this.expressionOptions = next;
            this.expressionTransition = { at: this.world.frame.timeMs, from };
        }
        const { sim, frame } = this.world;
        const activity = this.engine.activity(frame.timeMs);
        const observed = activity.observed && activity.freshness === 'fresh';
        const state = { ...frame.state, observed: observed ? 1 : 0, roamX: 0, roamY: 0, flip: 1,
            lit: observed ? 1 : frame.behaviour === 'doze' ? .18 : 1 };
        const key = JSON.stringify([state, frame.pod]);
        if (this.stillProjection?.key !== key) {
            const still = new pet_sim_js_1.PetSim(sim.p.map(p => [p.hx, p.hy]), 0xC0FFEE, sim.expressionVersion);
            const peers = frame.pod.filter(p => p.present);
            still.step(1 / 30, state, { motion: false, sensitivity: 1,
                podSlots: peers.length >= 3 ? peers.map(p => [[0, 2, 4, 1, 3, 5][p.slot], p.phase]) : undefined });
            this.stillProjection = { key, style: still.frame };
        }
        const still = (0, dot_field_js_1.projectDotField)(activity, frame.timeMs, true, 0, this.expressionOptions);
        const language = this.expressionOptions.textLanguage;
        const label = activity.authoritativePresence === 'needs_you' ? 'waiting'
            : activity.authoritativePresence === 'done' ? 'done' : activity.activityKind;
        return JSON.stringify({ ...frame, digest: (0, pet_sim_js_1.digest)(sim), state, style: { ...sim.frame, hollow: !observed }, activity,
            ...this.expression(), displayLanguage: language,
            displayPhrase: observed && label ? dot_glyphs_js_1.DOT_LABELS[language][label] : '',
            still: { ...still, style: { ...this.stillProjection.style, hollow: !observed }, state } });
    }
    /** Losing a producer invalidates outstanding coverage, never the creature. */
    disconnectEngine() {
        this.engine = new pet_engine_js_1.PetEngineTelemetry();
        this.world.voices = [];
        this.expressionActivity = this.engine.activity(this.world.frame.timeMs);
        this.expressionTransition = undefined;
    }
    interact(kind, x, y) { this.world.interact(kind, x, y); }
    interactions() { return JSON.stringify(this.world.interactions); }
    accept(packet) { this.world.acceptTelemetry(JSON.parse(packet)); }
    acceptLiveTail(text) {
        const packet = this.liveTape.readTail(text);
        if (!packet)
            return false;
        this.world.acceptTelemetry(packet);
        return true;
    }
    resetLiveInput() { this.liveTape.reset(); return this.resumeEngine(); }
    recording(withCheckpoint = false) { return JSON.stringify(this.world.recording(withCheckpoint)); }
    needsSegment() { return this.world.needsSegment; }
    prepareSegment() { this.segment = this.world.prepareSegment(); return JSON.stringify(this.segment.recording); }
    commitSegment() { if (!this.segment)
        throw new Error('No pet segment was prepared.'); this.segment.commit(); this.segment = undefined; }
    checkpoint() { return JSON.stringify(this.world.checkpoint()); }
    recordingChunk(index, completed = false) { return this.world.recordingChunk(index, completed); }
    restoreCheckpoint(text) {
        if (text.length > 512 * 1024)
            throw new Error('Pet checkpoint exceeds its size limit.');
        this.restoreRecording(JSON.stringify({ ...this.world.recording(false), checkpoint: JSON.parse(text) }));
    }
    restoreRecording(text) {
        if (text.length > 8 * 1024 * 1024)
            throw new Error('Native habitat exceeds 8 MiB.');
        const points = this.world.sim.p.map(p => [p.hx, p.hy]);
        this.world = pet_world_js_1.PetWorld.fromRecording(points, JSON.parse(text));
        this.liveTape.reset();
        this.segment = undefined;
        this.engineTick = Math.round(this.world.frame.timeMs * 30 / 1000);
        this.engine = new pet_engine_js_1.PetEngineTelemetry();
        this.expressionActivity = this.engine.activity(this.world.frame.timeMs);
        this.expressionTransition = undefined;
    }
    /** Resume a live host at the first unrecorded bucket. Keep every accepted
     * interval, but never present its last observed frame as current evidence. */
    resumeEngine() {
        this.world.resumeObservation();
        this.engineTick = Math.round(this.world.frame.timeMs * 30 / 1000);
        this.engine = new pet_engine_js_1.PetEngineTelemetry();
        this.expressionActivity = this.engine.activity(this.world.frame.timeMs);
        this.expressionTransition = undefined;
        this.world.voices = [];
        return this.world.frame.timeMs;
    }
    /** A rejected observation leaves the reducer exactly as it was, like a batch. */
    observeEngine(metadataJSON, timeMs) {
        const next = this.engine.clone();
        next.observe(JSON.parse(metadataJSON), timeMs);
        this.prepareExpression(next.activity(this.world.frame.timeMs));
        this.engine = next;
    }
    observeEngineBatch(metadataJSON, timeMs) {
        const events = JSON.parse(metadataJSON);
        if (!Array.isArray(events) || events.length > 64)
            throw new Error('Invalid Engine batch.');
        const next = this.engine.clone();
        for (const event of events)
            next.observe(event, timeMs);
        this.prepareExpression(next.activity(this.world.frame.timeMs));
        this.engine = next;
    }
    advanceEngine(timeMs, motion, waiting) {
        const target = Math.floor(timeMs * 30 / 1000 + 1e-8);
        if (!Number.isFinite(timeMs) || target < this.engineTick || target - this.engineTick > 300)
            throw new Error('Engine pet clock jump.');
        this.engine.confirmWaiting(timeMs, waiting);
        const voices = [];
        while (this.engineTick < target) {
            if ((this.engineTick + 1) % 12 === 0)
                this.world.acceptTelemetry(this.engine.bucket(Math.floor(this.engineTick / 12)));
            this.world.step(1 / 30, { motion, sensitivity: 1 });
            this.prepareExpression(this.engine.activity(this.world.frame.timeMs));
            voices.push(...this.world.voices);
            this.engineTick++;
        }
        this.world.voices = voices;
        this.prepareExpression(this.engine.activity(this.world.frame.timeMs));
    }
    terminal(width, height) {
        if (![width, height].every(n => Number.isSafeInteger(n) && n >= 1 && n <= 512))
            throw new Error('Invalid pet raster size.');
        const { sim, frame } = this.world, l = (0, pet_sim_js_1.layout)(width * 2, height * 4, frame.state);
        const cells = Array(width * height).fill(0), bits = [[1, 8], [2, 16], [4, 32], [64, 128]];
        sim.p.forEach((p, i) => {
            if (frame.state.observed < .92 && i % 2 === 1)
                return;
            const x = Math.round(l.ox + p.x * l.scale * l.flipX), y = Math.round(l.oy + p.y * l.scale);
            if (x >= 0 && x < width * 2 && y >= 0 && y < height * 4)
                cells[Math.floor(y / 4) * width + Math.floor(x / 2)] |= bits[y % 4][x % 2];
        });
        return JSON.stringify({ width, height, cells, timeMs: frame.timeMs, channel: frame.state.channel, arch: pet_sim_js_1.ARCH_OF[frame.state.channel],
            hollow: frame.state.observed < .92, dozing: frame.behaviour === 'doze', lit: frame.state.lit });
    }
    pcmChannels(voicesJSON, startSample, length, rate) {
        const p = (0, pet_audio_js_1.renderPetPCM)(JSON.parse(voicesJSON), startSample, length, rate);
        return [p.left, p.right];
    }
    pcm(voicesJSON, startSample, length, rate) {
        return JSON.stringify(this.pcmChannels(voicesJSON, startSample, length, rate).map(channel => Array.from(channel)));
    }
}
exports.PetNative = PetNative;

};
factories["pet-world"]=function(exports,require){
"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.PetWorld = exports.PET_MAX_SECONDS = void 0;
const model_js_1 = require("./model.js");
const pet_sim_js_1 = require("./pet-sim.js");
const pet_telemetry_js_1 = require("./pet-telemetry.js");
const pet_audio_js_1 = require("./pet-audio.js");
const HZ = 30;
var pet_sim_js_2 = require("./pet-sim.js");
Object.defineProperty(exports, "PET_MAX_SECONDS", { enumerable: true, get: function () { return pet_sim_js_2.PET_MAX_SECONDS; } });
const seedFor = (name) => (0xC0FFEE ^ (0, model_js_1.stableHash)(name)) >>> 0;
/** Fixed-tick creature controller. Wall clocks and pointer APIs belong to drivers.
 * Reconstructing with the same tape and interactions is also the seek operation.
 * Particle, behaviour and identity randomness never consume one another's stream. */
class PetWorld {
    sim;
    tapeLog;
    tapeHashes = [];
    get tape() { return this.tapeLog; }
    interactionLog;
    get interactions() { return this.interactionLog.map(e => ({ ...e })); }
    frame;
    voices = [];
    score = new pet_audio_js_1.PetScore();
    accumulator = 0;
    tick = 0;
    bucketIndex = -1;
    interactionIndex = 0;
    branchTick = -1;
    random = (0, pet_sim_js_1.mulberry32)(seedFor('behaviour'));
    behaviour = 'swim';
    until = 6;
    targetX = .35;
    targetY = -.08;
    x = 0;
    y = 0;
    flip = 1;
    lit = 1;
    lastActivity = 0;
    addressedAt = -Infinity;
    waitSince = -1;
    food = null;
    members = new Map();
    lastStill = '';
    origin;
    segmented = false;
    hasTelemetry = false;
    get startTimeMs() { return this.origin?.frame.timeMs ?? 0; }
    get endTimeMs() { return Math.max(this.frame.timeMs, (this.tapeLog.at(-1)?.simTimeMs ?? 0) + 400); }
    get needsSegment() {
        return !this.segmented && this.tapeLog.length < 1024 && this.interactionLog.length < 4096
            || this.bucketIndex >= 1024 || this.interactionIndex >= 4096;
    }
    constructor(points, tape = [], interactions = [], expressionVersion = 2, segmented = false) {
        if (tape.length > 216_000 || interactions.length > 100_000)
            throw new Error('Pet recording exceeds its input limit.');
        this.sim = new pet_sim_js_1.PetSim(points, 0xC0FFEE, expressionVersion);
        this.segmented = segmented;
        this.hasTelemetry = tape.length > 0;
        this.tapeLog = structuredClone([...tape]);
        this.interactionLog = structuredClone([...interactions]);
        for (let i = 0; i < this.tape.length; i++) {
            const b = this.tape[i];
            (0, pet_telemetry_js_1.validatePetBucket)(b);
            if (segmented ? i > 0 && b.sequence <= this.tape[i - 1].sequence : b.sequence !== i)
                throw new Error('World requires contiguous version 1 pet buckets.');
        }
        for (let i = 0; i < this.interactionLog.length; i++) {
            const e = this.interactionLog[i];
            if (!Number.isFinite(e.timeMs) || e.timeMs < 0 || i > 0 && e.timeMs < this.interactionLog[i - 1].timeMs
                || !['attention', 'food'].includes(e.kind) || !Number.isFinite(e.x) || !Number.isFinite(e.y)
                || Math.abs(e.x) > 1 || Math.abs(e.y) > 1)
                throw new Error('Invalid pet interaction.');
        }
        this.hashTape(0);
        this.frame = this.makeFrame(0);
        this.voices = this.score.voices(this.frame);
        if (segmented)
            this.origin = this.checkpoint();
    }
    checkpoint() {
        return structuredClone({ petCheckpointVersion: this.segmented ? 2 : 1,
            ...(this.segmented ? { historyStart: (this.origin?.tick ?? this.tick), hasTelemetry: this.hasTelemetry } : {}), history: this.historyDigest(),
            sim: this.sim.checkpoint(), score: this.score.checkpoint(), accumulator: this.accumulator,
            tick: this.tick, bucketIndex: this.bucketIndex, interactionIndex: this.interactionIndex, branchTick: this.branchTick,
            random: this.random.state(), behaviour: this.behaviour, until: this.until,
            targetX: this.targetX, targetY: this.targetY, x: this.x, y: this.y, flip: this.flip, lit: this.lit,
            lastActivity: this.lastActivity, addressedAt: Number.isFinite(this.addressedAt) ? this.addressedAt : null,
            waitSince: this.waitSince, food: this.food, members: [...this.members.values()], lastStill: this.lastStill,
            frame: this.frame, voices: this.voices });
    }
    recording(withCheckpoint = true, completed = false) {
        const tapeEnd = completed ? this.bucketIndex + 1 : this.tapeLog.length;
        const inputEnd = completed ? this.interactionIndex : this.interactionLog.length;
        const history = this.historyDigest(tapeEnd, inputEnd);
        return { petReplayVersion: this.segmented ? 2 : 1, expressionVersion: this.sim.expressionVersion,
            tape: this.tapeLog.slice(0, tapeEnd), interactions: this.interactionLog.slice(0, inputEnd).map(e => ({ ...e })),
            ...(this.origin ? { start: { ...structuredClone(this.origin), hasTelemetry: this.hasTelemetry, history } } : {}),
            ...(withCheckpoint ? { checkpoint: { ...this.checkpoint(), history } } : {}) };
    }
    static fromRecording(points, value) {
        const r = value;
        if (!r || ![1, 2].includes(r.petReplayVersion) || !Array.isArray(r.tape) || !Array.isArray(r.interactions))
            throw new Error('Invalid pet recording.');
        const version = r.expressionVersion === undefined ? 1 : r.expressionVersion;
        if (![1, 2].includes(version))
            throw new Error('Unsupported pet expression version.');
        if (r.checkpoint !== undefined && !r.checkpoint || r.start !== undefined && !r.start)
            throw new Error('Invalid pet checkpoint.');
        for (const c of [r.start, r.checkpoint])
            if (c && (c.sim?.expressionVersion ?? 1) !== version)
                throw new Error('Pet expression version does not match its checkpoint.');
        if (r.petReplayVersion === 1 && (r.start || r.checkpoint && r.checkpoint.petCheckpointVersion !== 1))
            throw new Error('Invalid legacy pet recording.');
        if (r.petReplayVersion === 2 && (!r.start || r.start.petCheckpointVersion !== 2 || r.start.historyStart !== r.start.tick))
            throw new Error('The recording segment is missing its starting checkpoint.');
        const first = r.start && PetWorld.restore(points, r.tape, r.interactions, r.start);
        const world = r.checkpoint ? PetWorld.restore(points, r.tape, r.interactions, r.checkpoint) : first ?? new PetWorld(points, r.tape, r.interactions, version);
        if (first) {
            if (!world.segmented || world.tick < first.tick || r.checkpoint && r.checkpoint.historyStart !== first.tick)
                throw new Error('Pet segment checkpoints do not agree.');
            world.origin = first.checkpoint();
        }
        return world;
    }
    /** Retire only consumed input. The exact origin makes each archived segment
     * independently replayable; no particle, random stream or score is reset. */
    prepareSegment() {
        const dropTape = Math.max(0, this.bucketIndex), dropInputs = this.interactionIndex, previous = this.origin;
        const tape = this.tapeLog.slice(dropTape), interactions = this.interactionLog.slice(dropInputs);
        const points = this.sim.p.map(p => [p.hx, p.hy]);
        const c = this.checkpoint();
        c.petCheckpointVersion = 2;
        c.historyStart = this.tick;
        c.hasTelemetry = this.hasTelemetry;
        c.bucketIndex -= dropTape;
        c.interactionIndex = 0;
        c.history = new PetWorld(points, tape, interactions, this.sim.expressionVersion, true).historyDigest();
        const next = PetWorld.restore(points, tape, interactions, c);
        next.origin = next.checkpoint();
        let committed = false;
        return { recording: next.recording(), archive: this.recording(true, true), commit: () => {
                if (committed || this.origin !== previous || this.tick < c.tick)
                    throw new Error('The pet recording segment has changed.');
                this.tapeLog.splice(0, dropTape);
                this.interactionLog.splice(0, dropInputs);
                this.bucketIndex -= dropTape;
                this.interactionIndex -= dropInputs;
                this.tapeHashes = [];
                this.hashTape(0);
                this.segmented = true;
                this.origin = next.origin;
                committed = true;
            } };
    }
    /** Lossless version 1 export, including the current pose and score. Drivers
     * consume all chunks synchronously on the world's owner before another tick.
     * Only a small slice is serialized inside an embedded runtime at a time. */
    recordingChunk(index, completed = false) {
        if (!Number.isSafeInteger(index) || index < 0)
            throw new Error('Invalid pet export cursor.');
        const size = 16, tapeEnd = completed ? this.bucketIndex + 1 : this.tapeLog.length;
        const inputEnd = completed ? this.interactionIndex : this.interactionLog.length;
        const tapes = Math.ceil(tapeEnd / size), inputs = Math.ceil(inputEnd / size);
        if (index === 0)
            return `{"petReplayVersion":${this.segmented ? 2 : 1},"expressionVersion":${this.sim.expressionVersion},"tape":[`;
        if (index <= tapes)
            return (index === 1 ? '' : ',') + JSON.stringify(this.tapeLog.slice((index - 1) * size, Math.min(tapeEnd, index * size))).slice(1, -1);
        if (index === tapes + 1)
            return '],"interactions":[';
        const part = index - tapes - 2;
        if (part < inputs)
            return (part === 0 ? '' : ',') + JSON.stringify(this.interactionLog.slice(part * size, Math.min(inputEnd, (part + 1) * size))).slice(1, -1);
        if (part === inputs) {
            const history = this.historyDigest(tapeEnd, inputEnd);
            return `]${this.origin ? ',"start":' + JSON.stringify({ ...this.origin, hasTelemetry: this.hasTelemetry, history }) : ''},"checkpoint":${JSON.stringify({ ...this.checkpoint(), history })}}`;
        }
        return null;
    }
    /** Prefix states preserve the original FNV checksum byte for byte. Live
     * appends and replacements hash only the changed suffix, so checkpointing
     * does not rescan hours of accepted telemetry on the world worker. */
    hashTape(from) {
        for (let i = from; i < this.tapeLog.length; i++)
            this.tapeHashes[i] = (0, model_js_1.stableHash)((i ? ',' : '') + JSON.stringify(this.tapeLog[i]), this.tapeHashes[i - 1] ?? (0, model_js_1.stableHash)('[['));
    }
    historyDigest(tapeEnd = this.tapeLog.length, inputEnd = this.interactionLog.length) {
        let hash = (0, model_js_1.stableHash)('],[', this.tapeHashes[tapeEnd - 1] ?? (0, model_js_1.stableHash)('[['));
        for (let i = 0; i < inputEnd; i++)
            hash = (0, model_js_1.stableHash)((i ? ',' : '') + JSON.stringify(this.interactionLog[i]), hash);
        return (0, model_js_1.stableHash)(']]', hash);
    }
    /** Hydrate a new world without stepping history. Validation finishes before
     * the caller receives it, so a corrupt checkpoint never mutates a live pet. */
    static restore(points, tape, interactions, value) {
        const c = value;
        const range = (n, low, high) => Number.isFinite(n) && n >= low && n <= high;
        const integer = (n, low, high) => Number.isSafeInteger(n) && range(n, low, high);
        if (!c || ![1, 2].includes(c.petCheckpointVersion) || JSON.stringify(c).length > 512 * 1024
            || c.petCheckpointVersion === 2 && (!integer(c.historyStart, 0, c.tick) || typeof c.hasTelemetry !== 'boolean')
            || !integer(c.tick, 0, pet_sim_js_1.PET_MAX_SECONDS * HZ) || !range(c.accumulator, -1e-8, 1 / HZ + 1e-8)
            || !integer(c.bucketIndex, -1, tape.length - 1) || !integer(c.interactionIndex, 0, interactions.length)
            || !integer(c.branchTick, -1, c.tick) || !integer(c.random, 0, 0xffffffff)
            || !['swim', 'dive', 'roll', 'breathe', 'drift', 'doze', 'wake'].includes(c.behaviour)
            || !range(c.until, 0, pet_sim_js_1.PET_MAX_SECONDS + 10) || ![c.targetX, c.targetY, c.x, c.y, c.flip].every(n => range(n, -1, 1))
            || !range(c.lit, 0, 1) || !range(c.lastActivity, 0, c.tick / HZ)
            || c.addressedAt !== null && !range(c.addressedAt, 0, c.tick / HZ)
            || !range(c.waitSince, -1, c.tick / HZ) || typeof c.lastStill !== 'string' || c.lastStill.length > 2048
            || !Array.isArray(c.members) || c.members.length > 6
            || c.members.some(m => !m || typeof m.id !== 'string' || !m.id || m.id.length > 4096
                || !integer(m.slot, 0, 5) || !range(m.phase, 0, Math.PI * 2) || typeof m.present !== 'boolean')
            || new Set(c.members.map(m => m.id)).size !== c.members.length || new Set(c.members.map(m => m.slot)).size !== c.members.length
            || c.food !== null && (!c.food || !range(c.food.time, 0, c.tick / HZ) || ![c.food.x, c.food.y].every(n => range(n, -1, 1)))
            || !c.frame || c.frame.timeMs !== c.tick * 1000 / HZ || c.frame.behaviour !== c.behaviour
            || !['none', 'orient', 'approach', 'call'].includes(c.frame.needs)
            || !range(c.frame.surface, -.9, -.8) || !range(c.frame.caustic, 0, 1)
            || c.frame.food !== null && (!c.frame.food || !range(c.frame.food.x, -1, 1) || !range(c.frame.food.y, -1, 1.21) || !range(c.frame.food.life, 0, 1))
            || JSON.stringify(c.frame.pod) !== JSON.stringify(c.members)
            || !Array.isArray(c.voices) || c.voices.length > 1024
            || c.voices.some(v => !v || typeof v.id !== 'string' || v.id.length > 256))
            throw new Error('Invalid pet world checkpoint.');
        (0, pet_sim_js_1.validatePetState)(c.frame.state);
        (0, pet_audio_js_1.renderPetPCM)(c.voices, 0, 0);
        const world = new PetWorld(points, tape, interactions, c.sim?.expressionVersion ?? 1, c.petCheckpointVersion === 2);
        if (c.petCheckpointVersion === 2) {
            world.hasTelemetry = c.hasTelemetry;
            world.origin = structuredClone(c);
        }
        if (c.history !== world.historyDigest()
            || c.bucketIndex >= 0 && world.tape[c.bucketIndex].simTimeMs > c.frame.timeMs + 1e-7
            // acceptTelemetry may fill past gaps after the most recent fixed tick.
            // Preserve that pending cursor exactly; it may lag only over empty gaps.
            || world.tape.some((b, i) => i > c.bucketIndex && b.simTimeMs <= c.frame.timeMs + 1e-7
                && (b.observed !== 0 || b.channel !== 'other' || b.errors || b.waiting || b.agentIds.length
                    || b.onsets.some(Boolean) || b.activeMs.some(Boolean)))
            || world.interactionLog.slice(0, c.interactionIndex).some(e => e.timeMs > c.frame.timeMs + 1e-7)
            || world.interactionLog[c.interactionIndex]?.timeMs <= c.frame.timeMs + 1e-7)
            throw new Error('Pet checkpoint does not match its recording.');
        const candidate = world.tape[c.bucketIndex];
        const telemetry = candidate && c.frame.timeMs < candidate.simTimeMs + candidate.durationMs ? candidate : undefined;
        if (JSON.stringify(c.frame.telemetry) !== JSON.stringify(telemetry))
            throw new Error('Pet checkpoint telemetry does not match its clock.');
        world.sim.restore(c.sim);
        world.score.restore(c.score);
        world.random.restore(c.random);
        world.accumulator = c.accumulator;
        world.tick = c.tick;
        world.bucketIndex = c.bucketIndex;
        world.interactionIndex = c.interactionIndex;
        world.branchTick = c.branchTick;
        world.behaviour = c.behaviour;
        world.until = c.until;
        world.targetX = c.targetX;
        world.targetY = c.targetY;
        world.x = c.x;
        world.y = c.y;
        world.flip = c.flip;
        world.lit = c.lit;
        world.lastActivity = c.lastActivity;
        world.addressedAt = c.addressedAt ?? -Infinity;
        world.waitSince = c.waitSince;
        world.food = structuredClone(c.food);
        world.members = new Map(c.members.map(m => [m.id, { ...m }]));
        world.lastStill = c.lastStill;
        // JSON omits undefined properties; keep the same frame shape as makeFrame.
        world.frame = { ...structuredClone(c.frame), telemetry: telemetry ? structuredClone(telemetry) : undefined };
        world.voices = structuredClone(c.voices);
        return world;
    }
    /** Resume observation beyond all already accepted live packets, without
     * replaying their sound or exposing a stale request as current work. */
    resumeObservation() {
        const last = this.tapeLog.at(-1), end = last ? (last.sequence + 1) * 12 : 0;
        if (!end || end - this.tick > 24)
            throw new Error('Only a live recording can resume observation.');
        while (this.tick < end)
            this.step(1 / 30, { motion: false, sensitivity: 1 });
        this.voices = [];
    }
    /** Branch at the current playhead; input is journalled for the next fixed tick.
     * A live touch never needs to re-simulate the creature's entire lifetime. */
    interact(kind, x, y) {
        if (!['attention', 'food'].includes(kind) || !Number.isFinite(x) || !Number.isFinite(y) || Math.abs(x) > 1 || Math.abs(y) > 1)
            throw new Error('Invalid pet interaction.');
        if (this.branchTick !== this.tick)
            this.interactionLog.splice(this.interactionIndex);
        this.branchTick = this.tick;
        this.interactionLog.push({ timeMs: (this.tick + 1) * 1000 / HZ, kind, x, y });
    }
    /** Accept a live source packet at the next 400ms boundary. The accepted tape,
     * including any missing intervals, is the exact replay authority for this host. */
    acceptTelemetry(input) {
        (0, pet_telemetry_js_1.validatePetBucket)(input);
        const sequence = Math.floor(this.tick / 12) + 1;
        if (this.tapeLog.length >= 216_000)
            throw new Error('Archive this pet recording before accepting more telemetry.');
        this.hasTelemetry = true;
        if (this.segmented) {
            const at = this.tapeLog.findIndex(b => b.sequence >= sequence);
            const index = at < 0 ? this.tapeLog.length : at;
            this.tapeLog.splice(index, at >= 0 && this.tapeLog[at].sequence === sequence ? 1 : 0, { ...structuredClone(input), sequence, simTimeMs: sequence * 400 });
            this.hashTape(index);
            return;
        }
        if (sequence >= 216_000)
            throw new Error('Archive the legacy recording before accepting more telemetry.');
        const changedFrom = Math.min(sequence, this.tapeLog.length);
        while (this.tapeLog.length <= sequence) {
            const at = this.tapeLog.length;
            this.tapeLog.push({ version: 1, sequence: at, simTimeMs: at * 400, durationMs: 400,
                activity: .12, coherence: .25, attention: 0, channel: 'other', observed: 0, roamX: 0, roamY: 0, flip: 1, lit: 1,
                onsets: Array(13).fill(0), activeMs: Array(13).fill(0), errors: 0, agentIds: [], waiting: false });
        }
        this.tapeLog[sequence] = { ...structuredClone(input), sequence, simTimeMs: sequence * 400 };
        this.hashTape(changedFrom);
    }
    /** dt is bounded so suspending a surface cannot cause an unbounded catch-up. */
    step(dt, opts = { motion: true, sensitivity: 1 }) {
        if (!Number.isFinite(dt) || dt < 0 || dt > 10)
            throw new Error('World dt must be in [0, 10] seconds.');
        this.accumulator += dt;
        this.voices = [];
        while (this.accumulator + 1e-10 >= 1 / HZ) {
            this.accumulator -= 1 / HZ;
            const time = ++this.tick / HZ;
            this.frame = this.makeFrame(time);
            this.voices.push(...this.score.voices(this.frame));
            if (!opts.motion) {
                this.frame.state = { ...this.frame.state, roamX: 0, roamY: 0, flip: 1, lit: this.behaviour === 'doze' ? .18 : 1 };
                this.frame.surface = -.86;
                this.frame.caustic = .5;
                if (this.frame.food)
                    this.frame.food.y = this.food.y;
            }
            const signature = JSON.stringify(this.frame.state);
            const peers = this.frame.pod.filter(p => p.present);
            const podSlots = peers.length >= 3 ? peers.map(p => [[0, 2, 4, 1, 3, 5][p.slot], p.phase]) : undefined;
            if (opts.motion || signature !== this.lastStill || podSlots)
                this.sim.step(1 / HZ, this.frame.state, { ...opts, podSlots });
            this.lastStill = signature;
        }
        return this.frame;
    }
    makeFrame(time) {
        const timeMs = this.tick * 1000 / HZ;
        while (this.bucketIndex + 1 < this.tape.length && this.tape[this.bucketIndex + 1].simTimeMs <= timeMs + 1e-7)
            this.bucketIndex++;
        const candidate = this.tape[this.bucketIndex];
        const telemetry = candidate && timeMs < candidate.simTimeMs + candidate.durationMs ? candidate : undefined;
        while (this.interactionIndex < this.interactionLog.length && this.interactionLog[this.interactionIndex].timeMs <= timeMs + 1e-7) {
            const e = this.interactionLog[this.interactionIndex++];
            this.addressedAt = time;
            this.lastActivity = time;
            this.targetX = e.x * .65;
            this.targetY = e.y * .65;
            if (e.kind === 'food')
                this.food = { time, x: e.x, y: e.y };
        }
        const busy = telemetry && telemetry.observed >= .92 && telemetry.activity > .2;
        if (busy)
            this.lastActivity = time;
        if (telemetry?.waiting) {
            if (this.waitSince < 0)
                this.waitSince = time;
        }
        else
            this.waitSince = -1;
        const waitingFor = this.waitSince < 0 ? 0 : time - this.waitSince;
        const needs = this.waitSince < 0 ? 'none' : waitingFor < 8 ? 'orient' : waitingFor < 25 ? 'approach' : 'call';
        const addressed = time - this.addressedAt < 2;
        if (this.behaviour === 'doze' && (busy || addressed)) {
            this.behaviour = 'wake';
            this.until = time + 2;
        }
        else if (!busy && !addressed && time - this.lastActivity >= 75)
            this.behaviour = 'doze';
        else if (time >= this.until) {
            const choices = ['swim', 'swim', 'dive', 'roll', 'breathe', 'drift'];
            this.behaviour = choices[Math.floor(this.random() * choices.length)];
            this.until = time + 3 + this.random() * 7;
            this.targetX = (this.random() * 2 - 1) * .75;
            this.targetY = this.behaviour === 'dive' ? .6 : this.behaviour === 'breathe' ? -.65 : (this.random() * 2 - 1) * .4;
        }
        const sleeping = this.behaviour === 'doze';
        if (sleeping) {
            this.targetY = .65;
            this.targetX = .15;
        }
        if (this.sim.expressionVersion === 1 && (needs === 'approach' || needs === 'call')) {
            this.targetX = 0;
            this.targetY = .3;
        }
        const move = sleeping ? .004 : this.behaviour === 'drift' ? .006 : .018;
        const dx = this.targetX - this.x;
        this.x += dx * move;
        this.y += (this.targetY - this.y) * move;
        this.flip += ((Math.abs(dx) < .015 ? this.flip < 0 ? -1 : 1 : dx < 0 ? -1 : 1) - this.flip) * .035;
        this.lit += ((sleeping ? .18 : 1) - this.lit) * .03;
        const wild = !this.hasTelemetry;
        const state = {
            activity: telemetry?.activity ?? (wild ? sleeping ? .05 : .18 : .12),
            coherence: telemetry?.coherence ?? (wild ? .94 : .25),
            attention: Math.max(telemetry?.attention ?? 0, addressed ? .85 : 0, this.sim.expressionVersion === 1 && needs === 'call' ? 1 : 0),
            // A touch changes orientation, never hides an instrumentation gap.
            channel: addressed ? 'human' : telemetry?.channel ?? 'other',
            observed: telemetry?.observed ?? (wild ? 1 : 0),
            roamX: this.x, roamY: this.y, flip: this.flip, lit: this.lit,
        };
        const activeIds = new Set(telemetry?.agentIds ?? []);
        for (const member of this.members.values())
            member.present = activeIds.has(member.id);
        for (const id of activeIds) {
            if (!this.members.has(id)) {
                const vacant = [...this.members.values()].find(m => !m.present);
                const slot = this.members.size < 6 ? this.members.size : vacant?.slot;
                if (slot !== undefined) {
                    if (this.members.size >= 6 && vacant)
                        this.members.delete(vacant.id);
                    this.members.set(id, { id, slot, phase: (0, pet_sim_js_1.mulberry32)(seedFor(`pod:${id}`))() * Math.PI * 2, present: true });
                }
            }
            const member = this.members.get(id);
            if (member)
                member.present = true;
        }
        return { timeMs, behaviour: this.behaviour, state, telemetry, needs,
            pod: [...this.members.values()].map(m => ({ ...m })),
            surface: -.86 + .012 * Math.sin(time * .8), caustic: .5 + .5 * Math.sin(time * .31),
            food: this.food && time - this.food.time < 5
                ? { x: this.food.x, y: this.food.y + (time - this.food.time) * .04, life: (0, model_js_1.clamp)(1 - (time - this.food.time) / 5, 0, 1) } : null };
    }
}
exports.PetWorld = PetWorld;

};
factories["model"]=function(exports,require){
"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.durationOf = exports.EMPTY_FILTERS = exports.CATEGORIES = void 0;
exports.eventMatches = eventMatches;
exports.errorOnsetOf = errorOnsetOf;
exports.stableHash = stableHash;
exports.quantile = quantile;
exports.clamp = clamp;
exports.formatTime = formatTime;
/** Versioned, vendor-neutral trace and signal contracts. All internal time is ms. */
exports.CATEGORIES = [
    'reasoning', 'tool', 'memory', 'code', 'filesystem', 'network', 'browser',
    'communication', 'agent', 'orchestration', 'error', 'human', 'other',
];
exports.EMPTY_FILTERS = { query: '', category: '', agent: '', model: '', tool: '', status: '' };
function eventMatches(e, f) {
    if (f.category && e.category !== f.category)
        return false;
    if (f.agent && e.agentId !== f.agent)
        return false;
    if (f.model && e.model !== f.model)
        return false;
    if (f.tool && e.tool !== f.tool)
        return false;
    if (f.status && e.status !== f.status)
        return false;
    if (f.query) {
        const q = f.query.toLowerCase();
        // Search is deliberately content-aware but runs only over locally retained fields.
        if (![e.name, e.id, e.agentId, e.model, e.tool, e.provider, e.category,
            JSON.stringify(e.attributes), JSON.stringify(e.payload), JSON.stringify(e.raw)].filter(Boolean).join(' ').toLowerCase().includes(q))
            return false;
    }
    return true;
}
const durationOf = (e) => Math.max(0, e.endTime - e.startTime);
exports.durationOf = durationOf;
/** An explicit failure receipt can arrive after a span began or ended. Keep
 * its timestamp distinct from the operation onset in every signal view. */
function errorOnsetOf(e) {
    const time = e.attributes['whalesong.error_onset_ms'];
    if (time === undefined)
        return e.startTime;
    if (typeof time !== 'number' || !Number.isFinite(time) || time < e.startTime)
        throw new Error('Invalid failure observation time.');
    return time;
}
function stableHash(text, seed = 2166136261) {
    let h = seed;
    for (let i = 0; i < text.length; i++) {
        h ^= text.charCodeAt(i);
        h = Math.imul(h, 16777619);
    }
    return h >>> 0;
}
function quantile(a, q) {
    if (!a.length)
        return 0;
    const s = [...a].sort((a, b) => a - b), x = Math.min(1, Math.max(0, q)) * (s.length - 1);
    return s[Math.floor(x)] + (s[Math.ceil(x)] - s[Math.floor(x)]) * (x % 1);
}
function clamp(x, lo, hi) { return Math.max(lo, Math.min(hi, x)); }
function formatTime(ms, precise = false) {
    const m = Math.floor(Math.max(0, ms) / 60000), s = Math.floor(Math.max(0, ms) / 1000) % 60;
    return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}${precise ? '.' + String(Math.floor(ms % 1000)).padStart(3, '0') : ''}`;
}

};
factories["pet-sim"]=function(exports,require){
"use strict";
// PetSim — the portable core of the Codewhale pet.
//
// This is a faithful, DOM-free port of the consort study's particle engine
// (grammar.js). The same code — same constants, same order of operations — is
// what the Rust, Swift and Kotlin cores implement. Four rules keep the view
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
Object.defineProperty(exports, "__esModule", { value: true });
exports.PetSim = exports.CHANNEL_INDEX = exports.CHANNELS = exports.ARCH_OF = exports.REST_STATE = exports.PET_MAX_SECONDS = void 0;
exports.validatePetState = validatePetState;
exports.mulberry32 = mulberry32;
exports.layout = layout;
exports.digest = digest;
exports.runTape = runTape;
exports.PET_MAX_SECONDS = 100 * 365 * 86_400;
exports.REST_STATE = {
    activity: 0.35, coherence: 0.8, attention: 0, channel: 'reasoning',
    observed: 1, roamX: 0, roamY: 0, flip: 1, lit: 1,
};
const lerp = (a, b, t) => a + (b - a) * t;
const clamp = (v, a = 0, b = 1) => Math.min(b, Math.max(a, v));
exports.ARCH_OF = {
    reasoning: 'gyre', memory: 'gyre',
    tool: 'strike', code: 'strike', filesystem: 'strike',
    network: 'cross', communication: 'cross', browser: 'cross',
    agent: 'pod', orchestration: 'pod',
    error: 'tear', human: 'address', other: 'drift',
};
exports.CHANNELS = [
    { key: 'reasoning', label: 'Model / reasoning', color: '#73c9b5', freq: 130.81, sustained: true, arch: 'gyre', form: 'gyre · rolling' },
    { key: 'tool', label: 'Tool calls', color: '#74aadd', freq: 261.63, sustained: false, arch: 'strike', form: 'strike · reaching' },
    { key: 'memory', label: 'Memory / RAG', color: '#b6a77f', freq: 195.99, sustained: true, arch: 'gyre', form: 'gyre · scanning' },
    { key: 'code', label: 'Code execution', color: '#9b9ed7', freq: 164.81, sustained: false, arch: 'strike', form: 'strike · along the body' },
    { key: 'filesystem', label: 'Filesystem', color: '#92b9c9', freq: 440.00, sustained: false, arch: 'strike', form: 'strike · fanning' },
    { key: 'network', label: 'Network / API', color: '#d3ac74', freq: 523.25, sustained: false, arch: 'cross', form: 'crossing · one way' },
    { key: 'browser', label: 'Browser / computer', color: '#9ea9df', freq: 349.23, sustained: false, arch: 'cross', form: 'crossing · a sweep' },
    { key: 'communication', label: 'Agent messages', color: '#83c5c9', freq: 293.66, sustained: false, arch: 'cross', form: 'crossing · two ways' },
    { key: 'agent', label: 'Subagent activity', color: '#b09acb', freq: 220.00, sustained: true, arch: 'pod', form: 'pod · peers' },
    { key: 'orchestration', label: 'Orchestration', color: '#6c8798', freq: 98.00, sustained: true, arch: 'pod', form: 'pod · hub' },
    { key: 'error', label: 'Errors / exceptions', color: '#e79186', freq: 185.00, sustained: false, arch: 'tear', form: 'torn · irregular' },
    { key: 'human', label: 'Human interaction', color: '#c2b787', freq: 391.99, sustained: false, arch: 'address', form: 'decision · junction' },
    { key: 'other', label: 'Unclassified', color: '#738492', freq: 146.83, sustained: false, arch: 'drift', form: 'drifting · unformed' },
];
exports.CHANNEL_INDEX = Object.fromEntries(exports.CHANNELS.map((c, i) => [c.key, i]));
function validatePetState(value) {
    const s = value;
    if (!s || typeof s !== 'object' || !Object.hasOwn(exports.CHANNEL_INDEX, s.channel)
        || ![s.activity, s.coherence, s.attention, s.observed, s.lit].every(n => Number.isFinite(n) && n >= 0 && n <= 1)
        || ![s.roamX, s.roamY, s.flip].every(n => Number.isFinite(n) && Math.abs(n) <= 1))
        throw new Error('Invalid pet state.');
}
const hex2rgb = (h) => [parseInt(h.slice(1, 3), 16), parseInt(h.slice(3, 5), 16), parseInt(h.slice(5, 7), 16)];
const RGB = exports.CHANNELS.map(c => hex2rgb(c.color));
const UNKNOWN_RGB = hex2rgb('#738492');
const REST_RGB = [122, 214, 240];
// mulberry32 — a 32-bit seeded PRNG tiny enough to port by hand correctly.
function mulberry32(seed) {
    let a = seed >>> 0;
    return Object.assign(() => {
        a = (a + 0x6D2B79F5) >>> 0;
        let t = a;
        t = Math.imul(t ^ (t >>> 15), t | 1);
        t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
        return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    }, { state: () => a, restore: (state) => {
            if (!Number.isSafeInteger(state) || state < 0 || state > 0xffffffff)
                throw new Error('Invalid pet random stream.');
            a = state;
        } });
}
/** Work reorganizes the same particles; no new random draws or invented facts.
 * These are expressive fields, not diagrams of unobserved network/file topology. */
function fieldTarget(q, t, act, att, key) {
    const u = q.s * 2 - 1, lane = q.pod - 2.5, a = q.s * Math.PI * 2;
    const flow = t * (.35 + act * .65);
    if (key === 'reasoning') {
        const ring = .34 + .105 * Math.cos(a * 3 + flow + lane * .18);
        return [ring * Math.cos(a * 2 + flow * .3), ring * Math.sin(a * 2 + flow * .3) * .7 + .10 * Math.sin(a * 3 + flow)];
    }
    if (key === 'memory')
        return [.46 * Math.cos(a + lane * .1 + flow * .25), lane * .082 + .052 * Math.sin(a * 2 + flow)];
    if (key === 'code')
        return [u * .57, lane * .066 + .12 * Math.sin(u * 7 + flow * 2 + q.pod * Math.PI / 3)];
    if (key === 'filesystem') {
        const branch = Math.max(0, (u + .3) / 1.3);
        return [u * .56, lane * .13 * branch + .025 * Math.sin(u * 8 - flow)];
    }
    if (key === 'tool') {
        const reach = .14 + (u + 1) * .20 + .04 * Math.sin(flow * 3 - u * 4);
        return [Math.cos(q.pod * Math.PI / 3) * reach, Math.sin(q.pod * Math.PI / 3) * reach * .8 + q.hy * .06];
    }
    if (key === 'browser')
        return [u * .56, lane * .083 + .035 * Math.sin(u * 5 - flow * 2)];
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
function gaitTarget(q, t, act, coh, att, key, work, podSlots, expressionVersion = 1) {
    const { hx, hy, ang, rad, tail, s, pod, jx, jy } = q;
    const omega = lerp(4.6, 5.2 + act * 2.8, work);
    const breath = 1 + Math.sin(t * 1.85) * lerp(0.048, 0.018, work);
    const flex = Math.sin(ang * 2.05 + t * omega) * lerp(0.042, 0.016 + act * 0.028, work) * (0.18 + 0.82 * tail);
    let px = Math.cos(ang + flex) * rad * breath;
    let py = Math.sin(ang + flex) * rad * breath;
    px += Math.sin(t * 0.33) * lerp(0.030, 0.014, work);
    py += Math.cos(t * 0.21) * lerp(0.018, 0.010, work);
    if (work < 0.02)
        return [px, py];
    const arch = exports.ARCH_OF[key] || 'drift';
    let gx = px, gy = py;
    if (arch === 'gyre') {
        if (key === 'memory') {
            const pulse = 1 + Math.sin(t * (2.4 + act * 1.6) - rad * 11) * (0.15 + act * 0.10);
            gx *= pulse;
            gy *= pulse;
        }
        else {
            const roll = Math.sin(t * (1.05 + act * 0.35)) * (0.48 + act * 0.32);
            const c = Math.cos(roll), sn = Math.sin(roll);
            gx = px * c - py * sn * 0.88;
            gy = px * sn * 0.88 + py * c;
        }
    }
    else if (arch === 'strike') {
        if (key === 'tool') {
            const rate = 2.7 + act * 2.1;
            const lunge = Math.pow(Math.max(0, Math.sin(t * rate)), 2);
            gx += lunge * 0.11;
            if (s > 0.60) {
                const reach = Math.pow(Math.max(0, Math.sin(t * rate + pod * 0.92)), 4) * (0.30 + act * 0.24);
                gx += Math.cos(ang) * reach;
                gy += Math.sin(ang) * reach;
            }
        }
        else if (key === 'code') {
            const rate = 3.2 + act * 1.8;
            const wave = Math.sin(t * rate - tail * 7.5);
            const bump = 0.11 + act * 0.08;
            gx += Math.cos(ang) * wave * bump;
            gy += Math.sin(ang) * wave * bump * 1.2;
            gx += Math.max(0, wave) * 0.07;
        }
        else {
            const rate = 2.15 + act * 1.5;
            const side = (pod % 2) * 2 - 1;
            const w = Math.pow(Math.max(0, Math.sin(t * rate + pod * 0.72)), 2);
            gx += w * 0.055;
            gy += side * w * (0.17 + act * 0.13);
        }
    }
    else if (arch === 'cross') {
        if (key === 'browser') {
            const band = ((t * (0.55 + act * 0.35)) % 1) * 1.28 - 0.64;
            const inBand = Math.max(0, 1 - Math.abs(hy - band) / 0.08);
            gx += inBand * (0.24 + act * 0.10);
            gy += inBand * 0.02;
        }
        else {
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
    }
    else if (arch === 'pod') {
        const n = 6, member = podSlots?.length ? podSlots[pod % podSlots.length] : undefined;
        const k = member ? member[0] : pod % n;
        const hub = key === 'orchestration' && k === 0;
        const spread = 0.30 + act * 0.11;
        const orbit = t * (0.55 + act * 0.28);
        if (hub) {
            gx = px * 0.70;
            gy = py * 0.70;
        }
        else {
            const slots = key === 'orchestration' ? n - 1 : n;
            const a = (key === 'orchestration' ? k - 1 : k) * (Math.PI * 2 / slots) + orbit + (member ? member[1] * .04 : 0);
            const sc = 0.34;
            gx = hx * sc + Math.cos(a) * spread * 1.28;
            gy = hy * sc + Math.sin(a) * spread * 0.80;
        }
    }
    else if (arch === 'tear') {
        const side = hx + hy < 0 ? -1 : 1;
        gx += side * (0.24 + (1 - coh) * 0.16);
        gy += side * 0.15;
        gx += Math.sin(t * 11.4 + s * 40) * (0.045 + act * 0.05);
        gy += Math.cos(t * 9.2 + s * 31) * (0.040 + act * 0.045);
    }
    else if (arch === 'address') {
        const face = 0.90 + att * 0.08;
        const th = 0.70;
        const z = (s - 0.5) * 0.42;
        let ax = hx * Math.cos(th) + z * Math.sin(th);
        let ay = hy;
        const disc = 0.48 * face;
        ax = lerp(ax, Math.cos(ang) * Math.min(0.36, rad + 0.06) * 0.95, disc);
        ay = lerp(ay, Math.sin(ang) * Math.min(0.36, rad + 0.06) * 1.08, disc);
        const grow = 1.20 + Math.sin(t * 1.65) * 0.055;
        gx = ax * grow;
        gy = ay * grow;
    }
    else {
        const mill = 0.13 + (1 - coh) * 0.10;
        gx = hx * 0.52 + Math.sin(t * 0.72 + jx) * mill;
        gy = hy * 0.52 + Math.cos(t * 0.54 + jy) * mill;
    }
    if (expressionVersion === 2) {
        const field = fieldTarget(q, t, act, att, key);
        if (field)
            [gx, gy] = field;
    }
    return [lerp(px, gx, work), lerp(py, gy, work)];
}
// Fixed reduced-motion clock per channel, so ticks still point along the gait.
const STILL_T = {
    reasoning: 1.15, memory: 0.42, tool: 0.30, code: 0.18, filesystem: 0.48,
    network: 0.72, browser: 0.95, communication: 0.58, agent: 1.25,
    orchestration: 0.85, error: 0.35, human: 0.05, other: 0.90,
};
class PetSim {
    expressionVersion;
    p;
    phase = 0;
    clock = 0;
    tear = 0;
    prev;
    col = [...REST_RGB];
    cur;
    frame = { r: REST_RGB[0], g: REST_RGB[1], b: REST_RGB[2], alpha: 0.3, hollow: false, channel: 'reasoning', arch: 'gyre', work: 0 };
    constructor(points, seed = 0xC0FFEE, expressionVersion = 2) {
        this.expressionVersion = expressionVersion;
        if (expressionVersion !== 1 && expressionVersion !== 2)
            throw new Error('Unsupported pet expression version.');
        const rand = mulberry32(seed);
        this.p = points.map(([hx, hy], i) => {
            const q = {
                x: hx, y: hy, vx: 0, vy: 0,
                s: rand(), jx: rand() * 6.283, jy: rand() * 6.283, pod: i % 6,
                hx, hy, ang: 0, rad: 0, tail: 0, tx: hx, ty: hy,
            };
            q.ang = Math.atan2(hy, hx);
            q.rad = Math.hypot(hx, hy);
            q.tail = clamp(((-hx - hy) * 0.5 + 0.22) / 0.62);
            return q;
        });
        this.cur = this.prev = exports.CHANNEL_INDEX['reasoning'];
    }
    checkpoint() {
        return { version: 1, expressionVersion: this.expressionVersion, body: this.p.map(p => [p.hx, p.hy, p.s]),
            particles: this.p.map(p => [p.x, p.y, p.vx, p.vy, p.jx, p.jy, p.tx, p.ty]),
            phase: this.phase, clock: this.clock, tear: this.tear, previous: this.prev, current: this.cur,
            color: [...this.col], frame: { ...this.frame } };
    }
    /** Restore into a newly constructed sim. Authored body and seeded particle
     * identity must match exactly; a checkpoint cannot replace the whale. */
    restore(value) {
        const c = value;
        const inRange = (n, low, high) => Number.isFinite(n) && n >= low && n <= high;
        if (!c || c.version !== 1 || c.expressionVersion !== undefined && ![1, 2].includes(c.expressionVersion) || (c.expressionVersion ?? 1) !== this.expressionVersion || !Array.isArray(c.body) || c.body.length !== this.p.length
            || c.body.some((v, i) => !Array.isArray(v) || v.length !== 3 || v[0] !== this.p[i].hx || v[1] !== this.p[i].hy || v[2] !== this.p[i].s)
            || !Array.isArray(c.particles) || c.particles.length !== this.p.length
            || c.particles.some(v => !Array.isArray(v) || v.length !== 8 || v.some((n, i) => !inRange(n, i === 4 || i === 5 ? 0 : -8, i === 4 || i === 5 ? 2 * exports.PET_MAX_SECONDS : 8)))
            || !inRange(c.phase, 0, exports.PET_MAX_SECONDS) || !inRange(c.clock, 0, exports.PET_MAX_SECONDS) || !inRange(c.tear, 0, 1)
            || ![c.previous, c.current].every(n => Number.isInteger(n) && n >= 0 && n < exports.CHANNELS.length)
            || !Array.isArray(c.color) || c.color.length !== 3 || c.color.some(n => !inRange(n, 0, 255))
            || !c.frame || ![c.frame.r, c.frame.g, c.frame.b].every(n => inRange(n, 0, 255))
            || !inRange(c.frame.alpha, 0, 1) || !inRange(c.frame.work, 0, 1) || typeof c.frame.hollow !== 'boolean'
            || c.frame.channel !== exports.CHANNELS[c.current].key || c.frame.arch !== exports.CHANNELS[c.current].arch)
            throw new Error('Invalid pet particle checkpoint.');
        this.phase = c.phase;
        this.clock = c.clock;
        this.tear = c.tear;
        this.prev = c.previous;
        this.cur = c.current;
        this.col = [...c.color];
        this.frame = { ...c.frame };
        this.p.forEach((p, i) => { [p.x, p.y, p.vx, p.vy, p.jx, p.jy, p.tx, p.ty] = c.particles[i]; });
    }
    /** Advance the sim by dt seconds under `state`. Identical math on every port. */
    step(dt, state, opts) {
        const S = (v) => lerp(0.5, v, opts.sensitivity);
        const act = S(state.activity), coh = S(state.coherence), att = S(state.attention);
        const seen = S(state.observed === undefined ? 1 : state.observed);
        const motion = opts.motion ? 1 : 0;
        this.phase += dt * (0.18 + act * 0.55) * motion;
        this.clock += dt * (opts.motion ? 1 : 0);
        if (exports.CHANNEL_INDEX[state.channel] !== undefined)
            this.cur = exports.CHANNEL_INDEX[state.channel];
        const shown = this.cur;
        const ch = exports.CHANNELS[shown];
        const work = clamp((act - 0.16) / 0.18);
        const wander = lerp(0.32, 1, Math.pow(1 - coh, 1.15));
        if (shown !== this.prev) {
            if (shown === exports.CHANNEL_INDEX['error'])
                this.tear = 1;
            this.prev = shown;
        }
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
            q.tx = tx;
            q.ty = ty;
            if (!opts.motion) {
                q.x = tx;
                q.y = ty;
                q.vx = 0;
                q.vy = 0;
                continue;
            }
            q.vx += (tx - q.x) * pull * dt;
            q.vy += (ty - q.y) * pull * dt;
            q.vx *= 0.90;
            q.vy *= 0.90;
            q.x += q.vx * dt * (opts.motion ? 2.6 : 8);
            q.y += q.vy * dt * (opts.motion ? 2.6 : 8);
        }
        // ---- visual encoding: the parts of "the same view" that are not motion
        const want = work > 0.35 ? RGB[shown] : REST_RGB;
        const k = opts.motion ? Math.min(1, dt * 2.6) : 1;
        for (let c = 0; c < 3; c++)
            this.col[c] += (lerp(UNKNOWN_RGB[c], want[c], seen) - this.col[c]) * k;
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
exports.PetSim = PetSim;
/** Body-space → renderer-space. Renderers place each dot at (lx,ly) in pixels/cells. */
function layout(w, h, state) {
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
function digest(sim) {
    const W = 64, H = 32;
    const grid = new Uint8Array(W * H);
    for (const q of sim.p) {
        const cx = Math.floor((q.x + 0.66) / 1.32 * W);
        const cy = Math.floor((q.y + 0.66) / 1.32 * H);
        if (cx >= 0 && cx < W && cy >= 0 && cy < H)
            grid[cy * W + cx] = Math.min(255, grid[cy * W + cx] + 1);
    }
    // FNV-1a 64 over the grid plus the frame encoding (rgb, hollow, alpha byte)
    // Two unsigned halves also work in embedded engines without BigInt.
    // FNV's prime is (256 << 32) + 435; these products stay below 2^42,
    // so every intermediate integer is exactly representable by a JS number.
    let hi = 0xcbf29ce4, lo = 0x84222325;
    const mix = (b) => {
        lo = (lo ^ (b & 0xff)) >>> 0;
        const product = lo * 435;
        hi = (hi * 435 + lo * 256 + Math.floor(product / 4294967296)) >>> 0;
        lo = product >>> 0;
    };
    for (const v of grid)
        mix(v);
    mix(Math.round(sim.frame.r));
    mix(Math.round(sim.frame.g));
    mix(Math.round(sim.frame.b));
    mix(Math.round(sim.frame.alpha * 255));
    mix(sim.frame.hollow ? 1 : 0);
    return hi.toString(16).padStart(8, '0') + lo.toString(16).padStart(8, '0');
}
/** Shared tape runner. `rows` are parsed tape.tsv lines. */
function runTape(sim, rows, opts) {
    const out = [];
    let f = 0;
    for (const row of rows) {
        const c = row.split('\t');
        if (c.length < 10 || c[0] === 'dt')
            continue;
        const st = {
            activity: +c[1], coherence: +c[2], attention: +c[3], channel: c[4],
            observed: +c[5], roamX: +c[6], roamY: +c[7], flip: +c[8], lit: +c[9],
        };
        sim.step(+c[0], st, opts);
        if (f++ % 30 === 0)
            out.push(`f${String(f - 1).padStart(4, '0')} ${digest(sim)} ${st.channel}`);
    }
    out.push(`final ${digest(sim)}`);
    return out;
}

};
factories["pet-telemetry"]=function(exports,require){
"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.PetLiveTape = exports.PET_BIN_MS = void 0;
exports.validatePetBucket = validatePetBucket;
exports.decodePetJSONL = decodePetJSONL;
exports.compilePetTelemetry = compilePetTelemetry;
exports.encodePetJSONL = encodePetJSONL;
exports.encodePetTSV = encodePetTSV;
const model_js_1 = require("./model.js");
const signal_js_1 = require("./signal.js");
const pet_sim_js_1 = require("./pet-sim.js");
/** One projection for imports, demos and recorded live snapshots. Times are ms.
 * These are aesthetic encodings of measured events, never model confidence. */
exports.PET_BIN_MS = 400;
function validatePetBucket(value) {
    (0, pet_sim_js_1.validatePetState)(value);
    const b = value;
    if (!b || typeof b !== 'object' || b.version !== 1 || !Number.isSafeInteger(b.sequence) || b.sequence < 0 || b.sequence > pet_sim_js_1.PET_MAX_SECONDS * 2.5
        || b.simTimeMs !== b.sequence * exports.PET_BIN_MS || b.durationMs !== exports.PET_BIN_MS
        || !model_js_1.CATEGORIES.includes(b.channel) || typeof b.waiting !== 'boolean'
        || !Array.isArray(b.agentIds) || b.agentIds.length > 250_000 || b.agentIds.some(id => typeof id !== 'string' || !id || id.length > 4096)
        || !Number.isSafeInteger(b.errors) || b.errors < 0 || b.errors > 250_000
        || !Array.isArray(b.onsets) || b.onsets.length !== 13 || b.onsets.some(n => !Number.isSafeInteger(n) || n < 0 || n > 250_000)
        || !Array.isArray(b.activeMs) || b.activeMs.length !== 13 || b.activeMs.some(n => !Number.isFinite(n) || n < 0 || n > 100_000_000))
        throw new Error('Invalid version 1 pet bucket.');
}
function decodePetJSONL(text) {
    if (text.length > 64 * 1024 * 1024)
        throw new Error('Pet tape exceeds 64 MiB.');
    const rows = text.split(/\r?\n/).filter(line => line.trim()).map(line => JSON.parse(line));
    if (rows.length > 216_000)
        throw new Error('Pet tape exceeds 24 hours.');
    return rows.map((row, i) => { validatePetBucket(row); if (row.sequence !== i)
        throw new Error('Non-contiguous pet tape.'); return row; });
}
/** A live file must advance before its contents count as a new observation.
 * Existing bytes, duplicate samples and a restarted sequence establish a
 * baseline; they never replay an old onset or human request. Drivers supply a
 * bounded tail and reset this cursor after suspension or a new attachment. */
class PetLiveTape {
    sequence;
    reset() { this.sequence = undefined; }
    readTail(text) {
        if (!text) {
            this.reset();
            return;
        }
        if (text.length > 262_144) {
            this.reset();
            throw new Error('Live pet input exceeds its tail limit.');
        }
        if (!text.endsWith('\n'))
            return;
        const line = text.trimEnd().split('\n').at(-1);
        if (!line)
            return;
        let packet;
        try {
            packet = JSON.parse(line);
            validatePetBucket(packet);
        }
        catch (error) {
            this.reset();
            throw error;
        }
        const previous = this.sequence;
        this.sequence = packet.sequence;
        if (previous === undefined || packet.sequence <= previous)
            return;
        return packet;
    }
}
exports.PetLiveTape = PetLiveTape;
const order = (a, b) => a < b ? -1 : a > b ? 1 : 0;
const keyOf = (e) => JSON.stringify([e.traceId, e.id]);
const isContainer = (e) => e.attributes['whalesong.container'] === true
    || e.attributes['codewhale.container'] === true;
/** Compile a single trace. Unknown-duration spans provide onsets, not occupancy.
 * Updates of the same trace/id replace earlier snapshots rather than double count.
 * An endpoint onset gets its own bucket; intervals use [start, end). */
function compilePetTelemetry(input, durationMs = 0, firstSequence = 0, originMs = 0) {
    if (input.length > 250_000)
        throw new Error('Pet input exceeds 250000 events.');
    if (!Number.isFinite(durationMs) || durationMs < 0)
        throw new Error('Invalid pet duration.');
    if (!Number.isSafeInteger(firstSequence) || firstSequence < 0)
        throw new Error('Invalid first pet bucket.');
    if (!Number.isFinite(originMs))
        throw new Error('Invalid pet clock origin.');
    durationMs = Math.max(0, durationMs - originMs);
    const unique = new Map();
    const traces = new Set();
    for (const e of input) {
        if (e.schemaVersion !== 1 || !e.id || !e.traceId || !model_js_1.CATEGORIES.includes(e.category)
            || !Number.isFinite(e.startTime) || !Number.isFinite(e.endTime)
            || e.startTime < 0 || e.endTime < e.startTime || !e.attributes)
            throw new Error('Invalid event-v1 pet input. Import through importTrace first.');
        traces.add(e.traceId);
        unique.set(keyOf(e), e);
    }
    if (traces.size > 1)
        throw new Error('Select one trace for the pet.');
    const parents = new Set([...unique.values()].filter(e => e.parentId).map(e => e.parentId));
    const events = [...unique.values()].filter(e => !isContainer(e)
        && !(e.category === 'orchestration' && parents.has(e.id)))
        .map(e => ({ ...e, startTime: e.startTime - originMs, endTime: (e.openEnded ? e.startTime : e.endTime) - originMs,
        attributes: e.attributes['whalesong.error_onset_ms'] === undefined ? e.attributes
            : { ...e.attributes, 'whalesong.error_onset_ms': (0, model_js_1.errorOnsetOf)(e) - originMs } }))
        .sort((a, b) => a.startTime - b.startTime || order(a.id, b.id));
    let lastOnset = 0;
    for (const e of events) {
        durationMs = Math.max(durationMs, e.endTime);
        lastOnset = Math.max(lastOnset, e.startTime);
    }
    const failures = events.filter(e => e.category === 'error' || e.status === 'error').map(model_js_1.errorOnsetOf).sort((a, b) => a - b);
    if (failures.length)
        lastOnset = Math.max(lastOnset, failures[failures.length - 1]);
    const count = Math.max(1, Math.ceil(durationMs / exports.PET_BIN_MS), Math.floor(lastOnset / exports.PET_BIN_MS) + 1);
    if (count - firstSequence > 216_000 || count > pet_sim_js_1.PET_MAX_SECONDS * 2.5)
        throw new Error('Pet replay exceeds 24 hours; select a shorter trace.');
    const index = new signal_js_1.IntervalIndex(events), result = [];
    const recent = [], names = new Map();
    let next = 0, expired = 0, nextFailure = 0;
    for (let sequence = firstSequence; sequence < count; sequence++) {
        const start = sequence * exports.PET_BIN_MS, end = start + exports.PET_BIN_MS;
        // A trailing window only: appending future events cannot rewrite earlier bins.
        while (next < events.length && events[next].startTime < end) {
            const e = events[next++];
            // A liveness pulse continues an operation; it is not a repeated tool call.
            if (e.attributes['whalesong.continuation'] === true)
                continue;
            recent.push(e);
            names.set(e.name, (names.get(e.name) ?? 0) + 1);
        }
        while (expired < recent.length && recent[expired].startTime < end - 12_000) {
            const name = recent[expired++].name, n = names.get(name) - 1;
            if (n)
                names.set(name, n);
            else
                names.delete(name);
        }
        const onsets = model_js_1.CATEGORIES.map(() => 0), activeMs = model_js_1.CATEGORIES.map(() => 0);
        const agents = new Set();
        while (nextFailure < failures.length && failures[nextFailure] < start)
            nextFailure++;
        let errors = 0, waiting = false, human = false;
        while (nextFailure < failures.length && failures[nextFailure] < end) {
            errors++;
            nextFailure++;
        }
        for (const e of index.query(start, end)) {
            if (e.startTime >= end)
                continue;
            const onset = e.startTime >= start, c = model_js_1.CATEGORIES.indexOf(e.category);
            const overlap = Math.max(0, Math.min(end, e.endTime) - Math.max(start, e.startTime));
            if (!onset && !overlap)
                continue;
            activeMs[c] += overlap;
            if (onset)
                onsets[c]++;
            if (e.agentId && !['unknown', 'unattributed'].includes(e.agentId))
                agents.add(e.agentId);
            if (e.category === 'human') {
                human = true;
                waiting ||= e.status === 'pending' || e.status === 'running' || e.attributes['whalesong.waiting'] === true;
            }
        }
        const total = activeMs.reduce((a, b) => a + b, 0), hits = onsets.reduce((a, b) => a + b, 0);
        const observed = total > 0 || hits > 0 || errors > 0;
        let dominant = model_js_1.CATEGORIES.indexOf('other');
        for (let c = 0; c < model_js_1.CATEGORIES.length; c++) {
            if (activeMs[c] > activeMs[dominant]
                || activeMs[c] === activeMs[dominant] && onsets[c] > onsets[dominant])
                dominant = c;
        }
        let repeated = 0;
        for (const n of names.values())
            if (n >= 4)
                repeated += n;
        const repeatDensity = repeated / Math.max(1, recent.length - expired);
        const channel = errors ? 'error' : human ? 'human' : agents.size >= 3 ? 'agent' : model_js_1.CATEGORIES[dominant];
        result.push({ version: 1, sequence, simTimeMs: start, durationMs: exports.PET_BIN_MS,
            activity: observed ? (0, model_js_1.clamp)(.28 + .38 * total / exports.PET_BIN_MS + .08 * hits, 0, 1) : .12,
            coherence: observed ? (0, model_js_1.clamp)(.92 - repeatDensity * .58 - Math.min(.55, errors * .18), .08, 1) : .25,
            attention: waiting ? .8 : human ? .65 : errors ? .45 : 0,
            channel, observed: observed ? 1 : 0, roamX: 0, roamY: 0, flip: 1, lit: 1,
            onsets, activeMs, errors, agentIds: [...agents].sort(order), waiting });
    }
    return result;
}
/** Flat state remains compatible with native readers; metadata preserves audio
 * onsets and peer identities. No prompts, tool arguments, or event names escape. */
function encodePetJSONL(buckets) {
    return buckets.map(b => JSON.stringify(b)).join('\n') + (buckets.length ? '\n' : '');
}
/** Legacy visual conformance interchange. Use JSONL to preserve audio metadata. */
function encodePetTSV(buckets) {
    return 'dt\tactivity\tcoherence\tattention\tchannel\tobserved\troamX\troamY\tflip\tlit\n'
        + buckets.map(b => [b.durationMs / 1000, b.activity, b.coherence, b.attention, b.channel,
            b.observed, b.roamX, b.roamY, b.flip, b.lit].join('\t')).join('\n') + '\n';
}

};
factories["signal"]=function(exports,require){
"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.IntervalIndex = void 0;
exports.buildPyramid = buildPyramid;
exports.chooseLevel = chooseLevel;
exports.binValue = binValue;
exports.intensity = intensity;
exports.totals = totals;
exports.onsetSeries = onsetSeries;
exports.autocorrelation = autocorrelation;
exports.periodogram = periodogram;
exports.unionDuration = unionDuration;
const model_js_1 = require("./model.js");
function emptyLevel(length, binMs) {
    const n = model_js_1.CATEGORIES.length * length;
    return { binMs, length, onsets: new Float64Array(n), activeMs: new Float64Array(n),
        outputTokens: new Float64Array(n), cost: new Float64Array(n), errors: new Float64Array(n), peak: new Float64Array(n) };
}
const FIELDS = ['onsets', 'activeMs', 'outputTokens', 'cost', 'errors'];
/** O(events + channels × bins), including long intervals. No span-length inner loop. */
function buildPyramid(events, requestedDuration, maxBins = 16_384) {
    if (!Number.isInteger(maxBins) || maxBins < 16 || maxBins > 1_048_576)
        throw new Error('maxBins must be an integer in [16, 1048576].');
    let duration = requestedDuration ?? 1;
    for (const e of events)
        duration = Math.max(duration, e.endTime, e.startTime, e.status === 'error' ? (0, model_js_1.errorOnsetOf)(e) : 0);
    if (!Number.isFinite(duration) || duration < 0)
        throw new Error('Signal duration must be finite and nonnegative.');
    duration = Math.max(1, duration);
    const binMs = 2 ** Math.ceil(Math.log2(Math.max(1, duration / (maxBins - 1))));
    const length = Math.floor(duration / binMs) + 1, fine = emptyLevel(length, binMs);
    const stride = length + 1;
    const activeDiff = new Float64Array(model_js_1.CATEGORIES.length * stride), tokenDiff = new Float64Array(model_js_1.CATEGORIES.length * stride);
    for (const e of events) {
        if (!Number.isFinite(e.startTime) || !Number.isFinite(e.endTime) || e.startTime < 0 || e.endTime < e.startTime)
            throw new Error(`Invalid interval for ${e.id}.`);
        const channel = model_js_1.CATEGORIES.indexOf(e.category);
        if (channel < 0)
            throw new Error(`Unknown category for ${e.id}.`);
        const a = Math.floor(e.startTime / binMs), b = Math.floor(e.endTime / binMs);
        const at = channel * length, diff = channel * stride;
        fine.onsets[at + a]++;
        fine.cost[at + a] += e.cost ?? 0;
        if (e.status === 'error')
            fine.errors[at + Math.floor((0, model_js_1.errorOnsetOf)(e) / binMs)]++;
        const d = e.endTime - e.startTime, tokens = e.outputTokens ?? 0;
        if (d === 0) {
            fine.outputTokens[at + a] += tokens;
            continue;
        }
        if (a === b) {
            fine.activeMs[at + a] += d;
            fine.outputTokens[at + a] += tokens;
        }
        else {
            const left = (a + 1) * binMs - e.startTime, right = e.endTime - b * binMs, tokenRate = tokens / d;
            fine.activeMs[at + a] += left;
            fine.activeMs[at + b] += right;
            fine.outputTokens[at + a] += left * tokenRate;
            fine.outputTokens[at + b] += right * tokenRate;
            if (b > a + 1) {
                activeDiff[diff + a + 1] += binMs;
                activeDiff[diff + b] -= binMs;
                tokenDiff[diff + a + 1] += binMs * tokenRate;
                tokenDiff[diff + b] -= binMs * tokenRate;
            }
        }
    }
    for (let c = 0; c < model_js_1.CATEGORIES.length; c++) {
        let active = 0, tokens = 0;
        for (let i = 0; i < length; i++) {
            active += activeDiff[c * stride + i];
            tokens += tokenDiff[c * stride + i];
            const at = c * length + i;
            fine.activeMs[at] = Math.max(0, fine.activeMs[at] + active);
            fine.outputTokens[at] = Math.max(0, fine.outputTokens[at] + tokens);
            fine.peak[at] = fine.activeMs[at] / binMs;
        }
    }
    const levels = [fine];
    while (levels.at(-1).length > 1) {
        const child = levels.at(-1), parent = emptyLevel(Math.ceil(child.length / 2), child.binMs * 2);
        for (let c = 0; c < model_js_1.CATEGORIES.length; c++)
            for (let i = 0; i < parent.length; i++) {
                const a = c * child.length + i * 2, b = a + 1, dst = c * parent.length + i, hasB = i * 2 + 1 < child.length;
                for (const key of FIELDS)
                    parent[key][dst] = child[key][a] + (hasB ? child[key][b] : 0);
                parent.peak[dst] = Math.max(child.peak[a], hasB ? child.peak[b] : 0);
            }
        levels.push(parent);
    }
    const p = { duration, channels: model_js_1.CATEGORIES, levels, calibration: { activity: 1, onsets: 1, tokens: 1, cost: 1 } };
    const reference = chooseLevel(p, duration / 1200);
    for (const metric of ['activity', 'onsets', 'tokens', 'cost']) {
        const positives = [];
        for (let c = 0; c < model_js_1.CATEGORIES.length; c++)
            for (let i = 0; i < reference.length; i++) {
                const v = binValue(reference, c, i, metric);
                if (v > 0)
                    positives.push(v);
            }
        p.calibration[metric] = Math.max(1e-9, (0, model_js_1.quantile)(positives, .95));
    }
    return p;
}
/** Choose the coarsest stored level no wider than one requested pixel interval. */
function chooseLevel(p, targetBinMs) {
    let result = p.levels[0];
    for (const level of p.levels) {
        if (level.binMs > targetBinMs)
            break;
        result = level;
    }
    return result;
}
function binValue(level, channel, bin, metric) {
    if (bin < 0 || bin >= level.length)
        return 0;
    const at = channel * level.length + bin;
    if (metric === 'activity')
        return level.activeMs[at] / level.binMs;
    if (metric === 'onsets')
        return level.onsets[at] * 1000 / level.binMs;
    if (metric === 'tokens')
        return level.outputTokens[at] * 1000 / level.binMs;
    return level.cost[at] * 1000 / level.binMs;
}
function intensity(value, reference) {
    return (0, model_js_1.clamp)(Math.log1p(value / Math.max(1e-9, reference) * 8) / Math.log(9), 0, 1);
}
/** Conserved totals: useful for tests and alternate native backends. */
function totals(level) {
    return Object.fromEntries(FIELDS.map(k => [k, level[k].reduce((s, x) => s + x, 0)]));
}
/** Sorted-start, max-end segment tree. Long root spans do not force a reverse
 * scan through every earlier event. Results are chronological and honor limits. */
class IntervalIndex {
    events;
    maxEnd;
    leafCount;
    constructor(events) {
        this.events = [...events].sort((a, b) => a.startTime - b.startTime || a.id.localeCompare(b.id));
        this.leafCount = 2 ** Math.ceil(Math.log2(Math.max(1, this.events.length)));
        this.maxEnd = new Float64Array(this.leafCount * 2).fill(-Infinity);
        for (let i = 0; i < this.events.length; i++)
            this.maxEnd[this.leafCount + i] = this.events[i].endTime;
        for (let i = this.leafCount - 1; i; i--)
            this.maxEnd[i] = Math.max(this.maxEnd[i * 2], this.maxEnd[i * 2 + 1]);
    }
    query(start, end, limit = Infinity) {
        if (!Number.isFinite(start) || !Number.isFinite(end) || end < start || limit <= 0)
            return [];
        let lo = 0, hi = this.events.length;
        while (lo < hi) {
            const mid = (lo + hi) >>> 1;
            if (this.events[mid].startTime <= end)
                lo = mid + 1;
            else
                hi = mid;
        }
        const bound = lo, found = [];
        const visit = (node, left, right) => {
            if (left >= bound || this.maxEnd[node] < start || found.length >= limit)
                return;
            if (right - left === 1) {
                const e = this.events[left];
                if (e && (e.endTime > start || e.startTime === e.endTime && e.startTime >= start))
                    found.push(e);
                return;
            }
            const mid = (left + right) >>> 1;
            visit(node * 2, left, mid);
            visit(node * 2 + 1, mid, right);
        };
        visit(1, 0, this.leafCount);
        return found;
    }
}
exports.IntervalIndex = IntervalIndex;
/** Sample an onset train into equal-width bins; nothing is inferred between impulses. */
function onsetSeries(events, start, end, n = 256, category) {
    const out = new Float64Array(n), span = Math.max(1e-6, end - start);
    for (const e of events)
        if ((!category || e.category === category) && e.startTime >= start && e.startTime < end) {
            const at = Math.floor((e.startTime - start) / span * n);
            if (at >= 0 && at < n)
                out[at]++;
        }
    return out;
}
/** Centered, variance-normalized autocorrelation; lag-zero is one unless constant. */
function autocorrelation(input, maxLag = 64) {
    const n = input.length;
    if (!n)
        return [];
    let mean = 0;
    for (let i = 0; i < n; i++)
        mean += input[i];
    mean /= n;
    const centered = Array.from(input, x => x - mean), energy = centered.reduce((s, v) => s + v * v, 0);
    const result = [];
    for (let lag = 0; lag <= Math.min(maxLag, n - 1); lag++) {
        let sum = 0;
        for (let i = 0; i < n - lag; i++)
            sum += centered[i] * centered[i + lag];
        result.push(energy > 1e-12 ? sum / energy : 0);
    }
    return result;
}
/** Hann-window periodogram of an actual uniformly sampled onset signal, DC removed. */
function periodogram(input, sampleHz) {
    const n = input.length, frequencies = [], power = [];
    if (n < 4 || sampleHz <= 0)
        return { frequencies, power, entropy: 0, peakHz: 0, sampleHz, resolutionHz: 0 };
    let mean = 0;
    for (let i = 0; i < n; i++)
        mean += input[i];
    mean /= n;
    const windowed = Array.from(input, (x, i) => (x - mean) * (.5 - .5 * Math.cos(2 * Math.PI * i / (n - 1))));
    const windowEnergy = Array.from({ length: n }, (_, i) => (.5 - .5 * Math.cos(2 * Math.PI * i / (n - 1))) ** 2).reduce((a, b) => a + b, 0);
    for (let k = 1; k <= Math.floor(n / 2); k++) {
        let re = 0, im = 0;
        for (let t = 0; t < n; t++) {
            const angle = 2 * Math.PI * k * t / n;
            re += windowed[t] * Math.cos(angle);
            im -= windowed[t] * Math.sin(angle);
        }
        frequencies.push(k * sampleHz / n);
        power.push((re * re + im * im) / (windowEnergy * sampleHz) * (n % 2 === 0 && k === n / 2 ? 1 : 2));
    }
    const sum = power.reduce((s, x) => s + x, 0);
    let entropy = 0;
    for (const x of power)
        if (x > 0 && sum > 0) {
            const p = x / sum;
            entropy -= p * Math.log2(p);
        }
    entropy = power.length > 1 ? entropy / Math.log2(power.length) : 0;
    const max = Math.max(...power);
    return { frequencies, power, entropy, peakHz: max > 1e-12 ? frequencies[power.indexOf(max)] : 0, sampleHz, resolutionHz: sampleHz / n };
}
function unionDuration(events, start = 0, end = Infinity) {
    const intervals = events.filter(e => e.endTime > e.startTime && e.endTime > start && e.startTime < end)
        .map(e => [Math.max(start, e.startTime), Math.min(end, e.endTime)]).sort((a, b) => a[0] - b[0]);
    let total = 0, left = 0, right = 0;
    for (const [a, b] of intervals) {
        if (a > right) {
            total += right - left;
            left = a;
            right = b;
        }
        else
            right = Math.max(right, b);
    }
    return total + right - left;
}

};
factories["pet-audio"]=function(exports,require){
"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.PetScore = void 0;
exports.renderPetPCM = renderPetPCM;
const pet_sim_js_1 = require("./pet-sim.js");
const model_js_1 = require("./model.js");
/** Core emits score events; WebAudio / AVAudioEngine only present their PCM.
 * Calling at any display cadence produces the same score when all world ticks
 * are supplied. Calling twice for a world tick cannot retrigger a voice. */
class PetScore {
    lastWindow = -1;
    lastSequence = -1;
    lastAddress = false;
    checkpoint() { return [this.lastWindow, this.lastSequence, this.lastAddress]; }
    restore(value) {
        if (!Array.isArray(value) || value.length !== 3
            || !value.slice(0, 2).every(n => Number.isSafeInteger(n) && n >= -1 && n <= pet_sim_js_1.PET_MAX_SECONDS * 2.5) || typeof value[2] !== 'boolean')
            throw new Error('Invalid pet score checkpoint.');
        [this.lastWindow, this.lastSequence, this.lastAddress] = value;
    }
    voices(frame) {
        const time = frame.timeMs / 1000, window = Math.floor((frame.timeMs + 1e-7) / 400);
        const out = [];
        const add = (id, frequency, duration, gain, pan = 0, delay = 0, kind = 'tone') => out.push({ id, start: time + delay, duration, frequency, gain, pan, kind });
        const t = frame.telemetry, fresh = t !== undefined && t.sequence !== this.lastSequence;
        if (fresh) {
            this.lastSequence = t.sequence;
            for (let c = 0; c < pet_sim_js_1.CHANNELS.length; c++) {
                const channel = pet_sim_js_1.CHANNELS[c], n = t.onsets[c];
                if (!n || channel.sustained || ['human', 'error'].includes(channel.key))
                    continue;
                add(`onset:${t.sequence}:${c}`, channel.freq, .24, .035 * Math.min(2, Math.sqrt(n)), (c / 12 - .5) * .7);
            }
            if (t.errors)
                add(`tear:${t.sequence}`, pet_sim_js_1.CHANNELS.find(c => c.key === 'error').freq, .22, .05, 0, 0, 'noise');
        }
        const address = frame.state.channel === 'human' && frame.state.attention > .5;
        if (address && (!this.lastAddress || fresh && t.onsets[11] > 0)) {
            const frequency = pet_sim_js_1.CHANNELS[11].freq;
            for (let i = 0; i < 3; i++)
                add(`address:${frame.timeMs}:${i}`, frequency * [1, 1.25, 1.5][i], .23, .032, 0, i * .14);
        }
        this.lastAddress = address;
        if (window !== this.lastWindow) {
            this.lastWindow = window;
            if (frame.state.observed >= .92) {
                const channel = pet_sim_js_1.CHANNELS.find(c => c.key === frame.state.channel);
                if (channel.sustained && frame.behaviour !== 'doze') {
                    const peers = frame.state.channel === 'agent' ? Math.max(1, frame.pod.filter(p => p.present).length) : 1;
                    for (let i = 0; i < peers; i++)
                        add(`sustain:${window}:${i}`, channel.freq * (1 + (i - (peers - 1) / 2) * .004), .44, (.02 + frame.state.activity * .02) / Math.sqrt(peers), peers === 1 ? 0 : i / (peers - 1) - .5);
                }
                if (frame.behaviour === 'doze' && window % 5 === 0) {
                    add(`heart:${window}:0`, 49, .3, .035);
                    add(`heart:${window}:1`, 49, .24, .023, 0, .33);
                }
                if (frame.needs === 'call' && window % 10 === 0)
                    add(`call:${window}`, pet_sim_js_1.CHANNELS[11].freq * 1.5, .38, .03);
            }
        }
        return out;
    }
}
exports.PetScore = PetScore;
/** Sample-addressed noise: no global RNG, identical samples when chunked/seeking. */
function noise(seed, sample) {
    let x = (seed + Math.imul(sample, 0x6D2B79F5)) >>> 0;
    x = Math.imul(x ^ x >>> 15, x | 1);
    x ^= x + Math.imul(x ^ x >>> 7, x | 61);
    return ((x ^ x >>> 14) >>> 0) / 2147483648 - 1;
}
function renderPetPCM(voices, startSample, length, sampleRate = 48_000) {
    if (!Number.isInteger(sampleRate) || sampleRate < 8000 || sampleRate > 96000
        || !Number.isSafeInteger(startSample) || startSample < 0 || !Number.isInteger(length) || length < 0 || length > sampleRate * 120)
        throw new Error('Invalid pet PCM range.');
    const left = new Float32Array(length), right = new Float32Array(length);
    for (const v of voices) {
        if (![v.start, v.duration, v.frequency, v.gain, v.pan].every(Number.isFinite)
            || v.start < 0 || v.duration <= 0 || v.duration > 10 || v.frequency <= 0 || v.frequency > sampleRate / 2
            || v.gain < 0 || v.gain > 1 || Math.abs(v.pan) > 1 || !['tone', 'noise'].includes(v.kind))
            throw new Error('Invalid pet voice.');
        const first = Math.max(startSample, Math.ceil(v.start * sampleRate));
        const last = Math.min(startSample + length, Math.ceil((v.start + v.duration) * sampleRate));
        const pan = (v.pan + 1) * Math.PI / 4, seed = (0xC0FFEE ^ (0, model_js_1.stableHash)(v.id)) >>> 0;
        for (let absolute = first; absolute < last; absolute++) {
            const age = absolute / sampleRate - v.start;
            const envelope = Math.min(1, age / .015, (v.duration - age) / .045);
            const sample = v.kind === 'noise' ? noise(seed, absolute) * Math.exp(-age * 14)
                : Math.sin(2 * Math.PI * v.frequency * age) * .88 + Math.sin(4 * Math.PI * v.frequency * age) * .12;
            const value = sample * Math.max(0, envelope) * v.gain, at = absolute - startSample;
            left[at] += value * Math.cos(pan);
            right[at] += value * Math.sin(pan);
        }
    }
    // Limiting is a presentation operation and cannot perturb voice scheduling.
    for (let i = 0; i < length; i++) {
        left[i] = (0, model_js_1.clamp)(left[i], -1, 1);
        right[i] = (0, model_js_1.clamp)(right[i], -1, 1);
    }
    return { left, right };
}

};
factories["pet-engine"]=function(exports,require){
"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.PetEngineTelemetry = exports.ENGINE_OWNER_STALE_MS = void 0;
const pet_sim_js_1 = require("./pet-sim.js");
const pet_telemetry_js_1 = require("./pet-telemetry.js");
// Engine emits a liveness pulse every 10 seconds for a running operation.
// Keep a small scheduling margin so one delayed pulse does not erase a valid
// long operation from the owner projection; after that, it becomes generic.
// The physics tape keeps its own, shorter coverage rule (see `pulse`).
exports.ENGINE_OWNER_STALE_MS = 12_000;
const ACTIVITY_KINDS = [
    'reading', 'editing', 'searching', 'testing', 'executing', 'browsing', 'computer',
    'memory', 'tool', 'thinking', 'responding', 'delegating',
];
const TURN_OUTCOMES = ['completed', 'interrupted', 'failed'];
const OPERATION_OUTCOMES = ['succeeded', 'failed', 'cancelled', 'denied'];
const APPROVAL_OUTCOMES = ['approved', 'denied', 'cancelled'];
function categoryFor(kind) {
    switch (kind) {
        case 'reading':
        case 'editing':
        case 'searching': return 'filesystem';
        case 'testing':
        case 'executing': return 'code';
        case 'browsing': return 'browser';
        case 'computer':
        case 'tool': return 'tool';
        case 'memory': return 'memory';
        case 'thinking': return 'reasoning';
        case 'responding': return 'communication';
        case 'delegating': return 'agent';
    }
}
/** Read-only reducer for the Engine owner's typed metadata. It keeps using the
 * existing pet telemetry tape and physics owner; it neither recognizes tool
 * names from content nor accepts transcript text, arguments, commands, or results. */
class PetEngineTelemetry {
    events = [];
    active = new Map();
    waiting;
    sequence = 0;
    lastTime = 0;
    lastObservedAt;
    turnId;
    turnOutcome;
    terminalAt;
    lastFailedTool;
    completedSpans = new Set();
    completedTurns = new Set();
    /** Ephemeral safe read projection. Span ids are retained only in this
     * reducer to correlate trusted lifecycle events and never leave the owner. */
    activity(at) {
        const observed = this.lastObservedAt !== undefined;
        const freshness = !observed ? 'missing'
            : at - this.lastObservedAt <= exports.ENGINE_OWNER_STALE_MS ? 'fresh' : 'stale';
        const fresh = freshness === 'fresh';
        const active = fresh
            ? [...this.active.values()]
                .filter(span => at >= span.startedAtMs && at - span.event.endTime <= exports.ENGINE_OWNER_STALE_MS)
                // The parent's own work leads; delegated agents are counted in
                // `parallelAgentCount` and only lead when nothing else is active.
                .sort((a, b) => Number(a.group === 'agent') - Number(b.group === 'agent')
                || b.startedAtMs - a.startedAtMs || a.key.localeCompare(b.key))
            : [];
        const agents = active.filter(span => span.group === 'agent').length;
        const terminalFresh = fresh && this.terminalAt !== undefined
            && at >= this.terminalAt && at - this.terminalAt <= exports.ENGINE_OWNER_STALE_MS;
        let authoritativePresence = 'unknown';
        if (fresh) {
            if (this.waiting)
                authoritativePresence = 'needs_you';
            else if (terminalFresh && this.turnOutcome === 'completed' && this.turnId)
                authoritativePresence = 'done';
            else if (terminalFresh)
                authoritativePresence = 'idle';
            else if (active.length > 0 || this.turnId)
                authoritativePresence = 'working';
        }
        const activityKind = fresh && authoritativePresence !== 'needs_you'
            && authoritativePresence !== 'done' && authoritativePresence !== 'idle'
            ? active[0]?.activityKind
                ?? (this.lastFailedTool && at >= this.lastFailedTool.failedAtMs
                    && at - this.lastFailedTool.failedAtMs <= exports.ENGINE_OWNER_STALE_MS
                    ? this.lastFailedTool.activityKind : null)
            : null;
        const failedAge = fresh && this.lastFailedTool
            && at >= this.lastFailedTool.failedAtMs
            && at - this.lastFailedTool.failedAtMs <= exports.ENGINE_OWNER_STALE_MS
            ? { activityKind: this.lastFailedTool.activityKind,
                ...(this.lastFailedTool.actionId ? { actionId: this.lastFailedTool.actionId } : {}),
                ageMs: at - this.lastFailedTool.failedAtMs }
            : null;
        const actionId = activityKind ? (active.length ? active[0].actionId : failedAge?.actionId) : undefined;
        const doneEffectId = authoritativePresence === 'done' && terminalFresh
            && this.turnOutcome === 'completed' && this.turnId ? this.turnId : null;
        return {
            schemaVersion: 1,
            sessionId: null,
            cursor: 0,
            observed,
            freshness,
            authoritativePresence,
            activityKind: activityKind ?? null,
            ...(actionId ? { actionId } : {}),
            observedAtMs: this.lastObservedAt ?? null,
            parallelAgentCount: fresh ? agents : 0,
            activeSpans: active.slice(0, 4).map(span => ({
                activityKind: span.activityKind,
                ...(span.actionId ? { actionId: span.actionId } : {}),
                startedAtMs: span.startedAtMs,
            })),
            turnId: this.turnId ?? null,
            turnOutcome: this.turnOutcome ?? null,
            doneEffectId,
            failedToolAge: failedAge,
        };
    }
    add(name, category, at, agentId = 'parent', continuation = false) {
        if (this.events.length >= 8192)
            throw new Error('Pet Engine observation window is full.');
        const event = {
            schemaVersion: 1,
            id: `engine:${this.sequence++}`,
            traceId: 'foreground',
            startTime: at,
            endTime: at,
            name,
            category,
            agentId,
            status: 'running',
            attributes: continuation ? { 'whalesong.continuation': true } : {},
        };
        this.events.push(event);
        return event;
    }
    start(key, kind, group, at, agentId = 'parent', actionId) {
        if (this.active.has(key))
            return;
        if (this.active.size >= 256)
            throw new Error('Too many active Engine pet spans.');
        const event = this.add(group === 'agent' ? 'agent' : group === 'thinking' ? 'thinking'
            : group === 'responding' ? 'assistant_message' : 'operation', categoryFor(kind), at, agentId);
        this.active.set(key, { key, activityKind: kind, actionId, startedAtMs: at, event, group });
    }
    pulse(key, at) {
        const span = this.active.get(key);
        if (!span)
            return;
        // A resumed stream does not assert tape coverage across its silent
        // interval; the span itself (and its start) stays active.
        if (at - span.event.endTime > pet_telemetry_js_1.PET_BIN_MS * 2) {
            const event = this.add(span.event.name, span.event.category, at, span.event.agentId, true);
            this.active.set(key, { ...span, event });
        }
        else {
            span.event.endTime = at;
        }
    }
    finish(key, at, status = 'success') {
        if (!this.active.has(key))
            return undefined;
        this.pulse(key, at);
        const span = this.active.get(key);
        if (!span)
            return undefined;
        span.event.endTime = at;
        span.event.status = status;
        this.active.delete(key);
        return span;
    }
    addOnce(set, id, maximum) {
        if (set.has(id))
            return false;
        set.add(id);
        while (set.size > maximum)
            set.delete(set.values().next().value);
        return true;
    }
    /** Transactional batch copy; validation errors cannot accept half a batch. */
    clone() {
        const next = new PetEngineTelemetry();
        const copy = (value) => JSON.parse(JSON.stringify(value));
        next.events = copy(this.events);
        const spans = new Map(next.events.map(event => [event.id, event]));
        next.active = new Map(Array.from(this.active, ([key, span]) => [key, {
                ...span,
                event: spans.get(span.event.id) ?? copy(span.event),
            }]));
        next.waiting = this.waiting ? spans.get(this.waiting.id) ?? copy(this.waiting) : undefined;
        next.sequence = this.sequence;
        next.lastTime = this.lastTime;
        next.lastObservedAt = this.lastObservedAt;
        next.turnId = this.turnId;
        next.turnOutcome = this.turnOutcome;
        next.terminalAt = this.terminalAt;
        next.lastFailedTool = this.lastFailedTool ? { ...this.lastFailedTool } : undefined;
        next.completedSpans = new Set(this.completedSpans);
        next.completedTurns = new Set(this.completedTurns);
        return next;
    }
    /** Mutates in place: clock, freshness and the event window change before
     * type-specific fields are checked. Callers that continue after a throw must
     * observe on a clone() and swap it in on success, as PetNative does. */
    observe(value, at) {
        if (!Number.isFinite(at) || at < this.lastTime || at > pet_sim_js_1.PET_MAX_SECONDS * 1000)
            throw new Error('Invalid Engine pet clock.');
        if (!value || typeof value !== 'object' || Array.isArray(value))
            throw new Error('Invalid Engine pet metadata.');
        const event = value;
        const allowed = ['event', 'index', 'channel', 'span_id', 'activity_kind', 'action_id', 'outcome', 'id', 'worker_status', 'turn_id', 'turn_outcome'];
        const stringFields = ['action_id', 'event', 'span_id', 'id', 'worker_status', 'turn_id', 'turn_outcome'];
        if (Object.keys(event).some(key => !allowed.includes(key)) || typeof event.event !== 'string'
            || Object.values(event).some(v => typeof v === 'string' && v.length > 256)
            || stringFields.some(key => event[key] !== undefined && typeof event[key] !== 'string')
            || event.action_id !== undefined && (typeof event.action_id !== 'string' || !event.action_id
                || /[\u0000-\u001f\u007f-\u009f]/.test(event.action_id)
                || [...event.action_id].reduce((bytes, char) => {
                    const cp = char.codePointAt(0);
                    return bytes + (cp <= 0x7f ? 1 : cp <= 0x7ff ? 2 : cp <= 0xffff ? 3 : 4);
                }, 0) > 256)
            || event.channel !== undefined && !['text', 'reasoning'].includes(event.channel)
            || event.activity_kind !== undefined && !ACTIVITY_KINDS.includes(event.activity_kind)
            || event.outcome !== undefined && (typeof event.outcome !== 'string'
                || (event.event === 'approval_resolved'
                    ? !APPROVAL_OUTCOMES.includes(event.outcome)
                    : event.event === 'operation_activity_completed'
                        ? !OPERATION_OUTCOMES.includes(event.outcome)
                        : true))
            || event.turn_outcome !== undefined && !TURN_OUTCOMES.includes(event.turn_outcome)
            || event.index !== undefined && (!Number.isSafeInteger(event.index) || event.index < 0))
            throw new Error('Invalid Engine pet metadata fields.');
        this.lastTime = at;
        this.lastObservedAt = at;
        this.events = this.events.filter(item => item.endTime >= at - 12_800);
        const required = (key) => {
            const value = event[key];
            if (typeof value !== 'string' || !value)
                throw new Error(`Missing Engine ${key}.`);
            return value;
        };
        const index = () => {
            if (!Number.isSafeInteger(event.index))
                throw new Error('Missing Engine index.');
            return String(event.index);
        };
        const activityKind = () => {
            if (!ACTIVITY_KINDS.includes(event.activity_kind))
                throw new Error('Missing Engine activity kind.');
            return event.activity_kind;
        };
        const startMessage = (key, kind, group) => {
            this.start(key, kind, group, at);
            this.waiting = undefined;
        };
        switch (event.event) {
            case 'turn_started': {
                const id = required('turn_id');
                if (this.turnId !== id) {
                    this.active.clear();
                    this.waiting = undefined;
                    this.lastFailedTool = undefined;
                    this.turnId = id;
                    this.turnOutcome = undefined;
                    this.terminalAt = undefined;
                }
                break;
            }
            case 'message_started':
                startMessage(`message:${index()}`, 'responding', 'responding');
                break;
            case 'thinking_started':
                startMessage(`thinking:${index()}`, 'thinking', 'thinking');
                break;
            case 'response_delta': {
                const reasoning = event.channel === 'reasoning';
                const key = `${reasoning ? 'thinking' : 'message'}:${index()}`;
                if (!this.active.has(key))
                    this.start(key, reasoning ? 'thinking' : 'responding', reasoning ? 'thinking' : 'responding', at);
                else
                    this.pulse(key, at);
                this.waiting = undefined;
                break;
            }
            case 'message_complete':
                this.finish(`message:${index()}`, at);
                break;
            case 'thinking_complete':
                this.finish(`thinking:${index()}`, at);
                break;
            case 'operation_activity_started': {
                const spanId = required('span_id');
                const kind = activityKind();
                if (!this.completedSpans.has(spanId))
                    this.start(`operation:${spanId}`, kind, 'operation', at, 'parent', event.action_id);
                this.waiting = undefined;
                break;
            }
            case 'operation_activity_completed': {
                const spanId = required('span_id');
                const kind = activityKind();
                if (!OPERATION_OUTCOMES.includes(event.outcome))
                    throw new Error('Missing Engine operation outcome.');
                const outcome = event.outcome;
                // A failure is recorded once, as its own onset event below; marking
                // the span `error` too would count it twice on the tape.
                const completed = this.finish(`operation:${spanId}`, at, outcome === 'succeeded' ? 'success' : 'unknown');
                if (completed && this.addOnce(this.completedSpans, spanId, 4096)) {
                    if (outcome === 'failed') {
                        this.add('operation_failed', 'error', at).status = 'error';
                        this.lastFailedTool = { activityKind: kind, actionId: completed.actionId, failedAtMs: at };
                    }
                    if (outcome === 'denied' || outcome === 'cancelled')
                        this.waiting = undefined;
                }
                break;
            }
            case 'tool_call_heartbeat':
                for (const [key, span] of this.active)
                    if (span.group === 'operation')
                        this.pulse(key, at);
                break;
            case 'approval_resolved': {
                required('id');
                if (!APPROVAL_OUTCOMES.includes(event.outcome))
                    throw new Error('Invalid Engine approval outcome.');
                this.waiting = undefined;
                break;
            }
            case 'agent_spawned': {
                const id = required('id');
                this.start(`agent:${id}`, 'delegating', 'agent', at, id);
                break;
            }
            case 'agent_progress': {
                const id = required('id');
                const key = `agent:${id}`;
                if (['completed', 'failed', 'cancelled', 'interrupted', 'budget_exhausted'].includes(event.worker_status)) {
                    this.finish(key, at);
                    break;
                }
                if (!this.active.has(key))
                    this.start(key, 'delegating', 'agent', at, id);
                else
                    this.pulse(key, at);
                break;
            }
            case 'agent_complete':
                this.finish(`agent:${required('id')}`, at);
                break;
            case 'approval_required':
            case 'user_input_required': {
                required('id');
                if (!this.waiting) {
                    this.waiting = this.add('human_request', 'human', at);
                    this.waiting.status = 'pending';
                }
                break;
            }
            case 'turn_complete': {
                const outcome = event.turn_outcome;
                if (!TURN_OUTCOMES.includes(outcome))
                    throw new Error('Missing Engine turn outcome.');
                const id = typeof event.turn_id === 'string' && event.turn_id.length ? event.turn_id : undefined;
                this.active.clear();
                this.waiting = undefined;
                this.turnId = id;
                // An outcome belongs to a turn. `/purge`, an edit rejection or a
                // session switch mid-turn completes with no turn id; recording the
                // outcome alone would break the projection invariant the Rust
                // contract checks (`turn_outcome` requires `turn_id`).
                this.turnOutcome = id ? outcome : undefined;
                this.terminalAt = at;
                if (outcome === 'completed' && id && this.addOnce(this.completedTurns, id, 256))
                    this.add('turn_completed', 'communication', at).status = 'success';
                break;
            }
            default: throw new Error('Unsupported Engine pet event.');
        }
    }
    /** The typed shell may extend a request already witnessed in Engine events,
     * but a mid-turn attach cannot invent a NeedsYou state. */
    confirmWaiting(at, waiting) {
        if (!waiting) {
            this.waiting = undefined;
            return;
        }
        if (this.waiting && at >= this.waiting.endTime) {
            this.waiting.endTime = at;
            this.lastObservedAt = at;
            this.lastTime = Math.max(this.lastTime, at);
        }
    }
    bucket(sequence) {
        const end = (sequence + 1) * pet_telemetry_js_1.PET_BIN_MS;
        const input = this.events.filter(event => event.startTime < end && event.endTime >= end - 12_400)
            .map(event => ({ ...event, endTime: Math.min(event.endTime, end) }));
        return (0, pet_telemetry_js_1.compilePetTelemetry)(input, end, sequence)[0];
    }
}
exports.PetEngineTelemetry = PetEngineTelemetry;

};
factories["dot-field"]=function(exports,require){
"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.dotFieldKey = dotFieldKey;
exports.textLanguage = textLanguage;
exports.projectDotField = projectDotField;
exports.blendDotFields = blendDotFields;
const model_js_1 = require("./model.js");
const dot_glyphs_js_1 = require("./dot-glyphs.js");
const dot_body_js_1 = require("./dot-body.js");
const dot_pixel_body_js_1 = require("./dot-pixel-body.js");
const TAU = Math.PI * 2;
const clamp = (n, lo = 0, hi = 1) => Math.max(lo, Math.min(hi, n));
const ease = (n) => { const u = clamp(n); return u * u * (3 - 2 * u); };
const GLYPH_POINTS = Object.fromEntries(Object.entries(dot_glyphs_js_1.DOT_GLYPHS).map(([language, glyphs]) => [language,
    Object.fromEntries(Object.entries(glyphs).map(([key, rows]) => [key,
        rows.flatMap((row, y) => [...row].flatMap((pixel, x) => pixel === '#' ? [[(x - 23.5) / 24, (y - 23.5) / 24]] : [])),
    ])),
]));
const HUES = {
    reading: 185, editing: 190, searching: 194, testing: 201, executing: 198,
    browsing: 195, computer: 198, memory: 180, tool: 195, thinking: 185,
    responding: 188, delegating: 192, waiting: 38, done: 180,
};
function verb(a, timeMs) {
    if (a.freshness !== 'fresh' || !a.observed)
        return null;
    if (a.authoritativePresence === 'needs_you')
        return 'waiting';
    if (a.authoritativePresence === 'done')
        return timeMs - (a.observedAtMs ?? timeMs) < 2600 ? 'done' : null;
    if (a.authoritativePresence === 'idle')
        return null;
    return a.activityKind;
}
function dotFieldKey(a) {
    return [a.freshness, a.authoritativePresence, a.activityKind, a.actionId,
        a.parallelAgentCount, a.doneEffectId, a.activeSpans.length === 0 && !!a.failedToolAge].join(':');
}
function pigment(hue, light) {
    const h = ((hue % 360) + 360) % 360 / 60, c = (1 - Math.abs(2 * light - 1)) * .62;
    const x = c * (1 - Math.abs(h % 2 - 1)), m = light - c / 2;
    const rgb = h < 1 ? [c, x, 0] : h < 2 ? [x, c, 0] : h < 3 ? [0, c, x]
        : h < 4 ? [0, x, c] : h < 5 ? [x, 0, c] : [c, 0, x];
    return rgb.map(v => Math.round((v + m) * 255));
}
function textLanguage(value = 'auto', interfaceLanguage = 'en') {
    const tag = (value === 'auto' ? interfaceLanguage : value).toLowerCase();
    return tag.startsWith('ja') ? 'ja' : /^(zh-hant|zh-tw|zh-hk|zh-mo)/.test(tag) ? 'zh-Hant'
        : tag.startsWith('zh') ? 'zh-Hans' : 'en';
}
function projectDotField(activity, timeMs, still = false, expressionAtMs = 0, options = {}) {
    const kind = verb(activity, timeMs), identity = activity.actionId || activity.activityKind || 'whale';
    const seed = (0, model_js_1.stableHash)(identity), variant = (seed % 997) / 997;
    const t = still ? 0 : timeMs / 1000, cycle = still ? .43 : (t / (2.8 + variant * .6)) % 1;
    const sweep = -.86 + cycle * 1.72, scanRow = Math.floor(cycle * 5) % 5;
    const bodyStyle = options.bodyStyle ?? 'smooth', language = textLanguage(options.textLanguage);
    const body = bodyStyle === 'pixel' ? dot_pixel_body_js_1.DOT_PIXEL_BODY : dot_body_js_1.DOT_BODY;
    const glyph = kind ? GLYPH_POINTS[language][kind] : null;
    const fresh = activity.freshness === 'fresh' && activity.observed;
    const failed = fresh && !activity.activeSpans.length && !!activity.failedToolAge;
    const phase = ((Math.max(0, timeMs - expressionAtMs) / 1000 + 1.7) % 8.4);
    const morph = !kind ? 0 : still ? 1 : kind === 'done'
        ? 1 - ease((timeMs - (activity.observedAtMs ?? timeMs) - 800) / 900)
        : ease(phase / 1.7) * (1 - ease((phase - 5.4) / .9));
    const points = [], materials = [];
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
                        * (.35 + .65 * Math.exp(-Math.pow((x - sweep) * 4, 2)));
                    break;
                case 'searching':
                case 'computer':
                    beam = Math.exp(-((x - lens[0]) ** 2 + (y - lens[1]) ** 2) * 24);
                    break;
                case 'editing':
                case 'responding':
                case 'testing':
                    beam = Math.exp(-Math.pow((x - sweep) * 12, 2));
                    break;
                case 'executing':
                case 'tool':
                    beam = (1 + Math.sin(x * 8 - y * 3 - cycle * TAU)) / 2;
                    break;
                case 'memory':
                    beam = Math.exp(-Math.pow((Math.hypot(x, y) - (1 - cycle)) * 12, 2));
                    break;
                case 'thinking':
                    beam = (1 + Math.cos(Math.atan2(y, x) - cycle * TAU)) / 2;
                    break;
                case 'delegating':
                    beam = (1 + Math.sin(y * Math.max(1, Math.min(6, activity.parallelAgentCount)) * 5 - cycle * TAU)) / 2;
                    break;
                case 'waiting':
                    beam = .2 + .2 * (1 + Math.sin(t * 1.5)) / 2;
                    break;
                case 'done':
                    beam = still ? .35 : clamp(1 - (timeMs - (activity.observedAtMs ?? timeMs)) / 1700);
                    break;
            }
            light = .66 + beam * .18;
            alpha = .84 + beam * .15;
            if (!still) {
                const px = x, py = y;
                x += .009 * Math.sin(py * 12 - t * 1.3) + .003 * Math.sin(px * 24 + py * 8 + t * 2.1);
                y += .007 * Math.sin(px * 13 + t * 1.05) + .003 * Math.cos(py * 26 - px * 6 - t * 1.7);
            }
            x = homeX + (x - homeX) * morph;
            y = homeY + (y - homeY) * morph;
            light = bodyLight + (light - bodyLight) * morph;
            alpha = .88 + (alpha - .88) * morph;
        }
        if (!still)
            light = clamp(light + .025 * Math.sin(hx * 10 - hy * 5 - t * 1.1), .5, .9);
        points.push([clamp(x, -.94, .94), clamp(y, -.94, .94)]);
        const hue = morph > 0 ? (failed ? 16 : kind ? HUES[kind] : 188) : 188;
        const base = bodyStyle === 'pixel' ? dot_pixel_body_js_1.DOT_PIXEL_BODY[i].slice(2)
            : pigment(185 + clamp((hy - hx + .45) / .9) * 70, light);
        const accent = pigment(hue + variant * 6 - 3, light);
        const color = !fresh ? [145, 166, 178] : base.map((v, k) => Math.round(v + (accent[k] - v) * morph));
        materials.push([...color, alpha]);
    }
    return { points, materials, morph, bodyStyle };
}
function blendDotFields(from, to, progress) {
    const mix = ease(progress);
    if (mix >= 1)
        return to;
    return {
        morph: from.morph + (to.morph - from.morph) * mix,
        bodyStyle: to.bodyStyle,
        points: to.points.map((p, i) => p.map((v, k) => from.points[i][k] + (v - from.points[i][k]) * mix)),
        materials: to.materials.map((p, i) => p.map((v, k) => from.materials[i][k] + (v - from.materials[i][k]) * mix)),
    };
}

};
factories["dot-glyphs"]=function(exports,require){
"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.DOT_GLYPHS = exports.DOT_LABELS = void 0;
exports.DOT_LABELS = {
    "en": {
        "reading": "read",
        "editing": "patch",
        "searching": "find",
        "testing": "verify",
        "executing": "run",
        "browsing": "Using the browser",
        "computer": "Using the computer",
        "memory": "Retrieving context",
        "tool": "tool",
        "thinking": "think",
        "responding": "Reply",
        "delegating": "agents underway",
        "waiting": "needs you",
        "done": "Done"
    },
    "zh-Hans": {
        "reading": "读取",
        "editing": "编辑",
        "searching": "搜索",
        "testing": "验证",
        "executing": "运行",
        "browsing": "使用浏览器",
        "computer": "操作电脑",
        "memory": "检索上下文",
        "tool": "调用工具",
        "thinking": "思考",
        "responding": "回复",
        "delegating": "智能体工作中",
        "waiting": "等你处理",
        "done": "完成"
    },
    "zh-Hant": {
        "reading": "讀取",
        "editing": "編輯",
        "searching": "搜尋",
        "testing": "驗證",
        "executing": "執行",
        "browsing": "使用瀏覽器",
        "computer": "操作電腦",
        "memory": "檢索上下文",
        "tool": "呼叫工具",
        "thinking": "思考",
        "responding": "回覆",
        "delegating": "代理程式工作中",
        "waiting": "等你處理",
        "done": "完成"
    },
    "ja": {
        "reading": "読込",
        "editing": "パッチ",
        "searching": "検索",
        "testing": "検証",
        "executing": "実行",
        "browsing": "ブラウザーを操作中",
        "computer": "コンピューターを操作中",
        "memory": "コンテキストを検索中",
        "tool": "ツール",
        "thinking": "思考",
        "responding": "返信",
        "delegating": "サブエージェント実行中",
        "waiting": "応答待ち",
        "done": "完了"
    }
};
exports.DOT_GLYPHS = {
    'en': {
        reading: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '..........................................##....',
            '..........................................##....',
            '..........................................##....',
            '..........................................##....',
            '..........................................##....',
            '....##..##....####.......####........####.##....',
            '....######...######....#######......########....',
            '....###.....##....##...#....###....###...###....',
            '....##.....##.....##.........##...###.....##....',
            '....##.....#########.......####...###.....##....',
            '....##.....#########....#######...##......##....',
            '....##.....##..........##....##...##......##....',
            '....##.....###........##.....##...###.....##....',
            '....##......###.......###...###....###...###....',
            '....##.......#######...########....#########....',
            '....##........#####.....###..##......###..##....',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        editing: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '.....................................#..........',
            '....................................##..........',
            '.......................##...........##..........',
            '.......................##...........##..........',
            '...##.###......####..######...####..##.####.....',
            '...#######...######...###....#####..#######.....',
            '...##....##.......##...##...##......###...##....',
            '...##....##.......##...##...##......##....##....',
            '...##....##...######...##...##......##....##....',
            '...##....##..##...##...##...##......##....##....',
            '...##....##..##...##...##...##......##....##....',
            '...#######...#######...####..######.##....##....',
            '...##.###.....###.##....###...####..##....##....',
            '...##...........................................',
            '...##...........................................',
            '...##...........................................',
            '...##...........................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        searching: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '......####......................................',
            '.....#####..###..........................##.....',
            '.....###....###..........................##.....',
            '.....##.....##...........................##.....',
            '....###..................................##.....',
            '....###..................................##.....',
            '...#######..###....##...####........####.##.....',
            '...#######..###....##.#######.....#########.....',
            '....###.....###....#####..###....####..####.....',
            '....###.....###....###.....##....###.....##.....',
            '....###.....###....###.....##...###......##.....',
            '....###.....###....###.....##...###......##.....',
            '....###.....###....###.....##...###......##.....',
            '....###.....###....###.....##...###......##.....',
            '....###.....###....###.....##...###......##.....',
            '....###.....###....###.....##....###.....##.....',
            '....###.....###....###.....##....####..####.....',
            '....###.....###....###.....##.....#########.....',
            '....###.....###....###.....##......####..##.....',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        testing: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '...........................##....##.............',
            '...........................##...##..............',
            '................................#...............',
            '................................#...............',
            '....##....##...###...##.##.##.#######....##.....',
            '.....#....#...##.##..#####.##..###..#....#......',
            '.....##...#..##...##.###...##...#...##...#......',
            '.....##..##..##...##.##....##...#...##..##......',
            '......#..##..#######.##....##...#....#..##......',
            '......##.#...##......##....##...#....##.#.......',
            '......####...##......##....##...#.....#.#.......',
            '.......###....##..#..##....##...#.....###.......',
            '.......##......####..##....##...#.....##........',
            '......................................##........',
            '......................................##........',
            '.....................................##.........',
            '....................................##..........',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        executing: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '....###..####..###......###....###...####.......',
            '....###.####...###......###....###.#######......',
            '....########...###......###....############.....',
            '....#####......###......###....#####....###.....',
            '....####.......###......###....###......###.....',
            '....###........###......###....###......###.....',
            '....###........###......###....###......###.....',
            '....###........###......###....###......###.....',
            '....###........###......###....###......###.....',
            '....###........###......###....###......###.....',
            '....###........###......###....###......###.....',
            '....###........####....####....###......###.....',
            '....###.........###########....###......###.....',
            '....###.........#######.###....###......###.....',
            '....###..........#####..###....###......###.....',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        browsing: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '..........#....#................................',
            '..........#....#.......#........................',
            '..........#....#................................',
            '..........#....#..###..#..#.##....####..........',
            '..........#....#..#....#..##.##..#..#...........',
            '..........#....#..##...#..#...#..#..#...........',
            '..........#....#....##.#..#...#..####...........',
            '...........#..##....##.#..#...#..#..............',
            '...........####...###..#..#...#..#####..........',
            '.................................#...#..........',
            '.................................#...#..........',
            '.................................####...........',
            '................................................',
            '................................................',
            '....................#...........................',
            '....................#...........................',
            '................#...#...........................',
            '...............####.#.##....###.................',
            '................#...##.##..#..##................',
            '................#...#...#..#####................',
            '................#...#...#..#....................',
            '................##..#...#..##...................',
            '.................##.#...#...###.................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '...#............................................',
            '...#............................................',
            '...#............................................',
            '...####...#.#...###..##..#..##.###...###..#.#...',
            '...##..#..##...#..##..#..##.#..#....#..##.##....',
            '...#...#..#....#...#..#.#.#.#..##...#####.#.....',
            '...#...#..#....#...#..#.#.#.#....##.#.....#.....',
            '...#..##..#....#..##...##.###....##.##....#.....',
            '...####...#.....###....##..#...###...###..#.....',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        computer: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '............#...#......#........................',
            '............#...#...............................',
            '............#...#..###.#..###...####............',
            '............#...#..#...#..#..#..#.#.............',
            '............#...#..##..#..#..#..###.............',
            '............#..#.....#.#..#..#..#...............',
            '.............###...##..#..#..#..###.............',
            '...................................#............',
            '................................###.............',
            '................................................',
            '................................................',
            '................................................',
            '.....................#..........................',
            '.................#...#..........................',
            '................###..###...###..................',
            '.................#...#..#..#..#.................',
            '.................#...#..#.#####.................',
            '.................#...#..#..#....................',
            '.................##..#..#..###..................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '..................................#.............',
            '...###..###...###.##..###...#..#.###..###..###..',
            '...#....#..#..#..#..#.#..#..#..#..#...#..#.#....',
            '..##...##..#..#..#..#.#..#..#..#..#..#####.#....',
            '...#....#..#..#..#..#.#..#..#..#..#...#....#....',
            '...###..###...#..#..#.###...####..##..###..#....',
            '......................#.........................',
            '......................#.........................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        memory: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '.....................#..........#...............',
            '.....###.......#................................',
            '.....#..#.###.###.##.#.###.#..#.#.###..###......',
            '.....#..#...#..#..#..#...#....#.#.#.#....#......',
            '.....###.####..#..#..#####..#...#.#.#..#.#......',
            '.....#.#..#....#..#..#.#....##..#.#.#..#........',
            '.....#..#.###..##.#..#.###..##..#.#.#..###......',
            '......................................#..#......',
            '.......................................###......',
            '................................................',
            '................................................',
            '................................................',
            '........................#...........#...........',
            '..........###.###..###.###.###.#..####..........',
            '..........#...#.#..#.#..#....#..##..#...........',
            '.........#...#.....#.#..#.####..##..#...........',
            '..........#...#.#..#.#..#..#....##..#...........',
            '..........###.###..#.#..##.###.#..#.##..........',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        tool: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '.........................................##.....',
            '.........................................###....',
            '.........................................###....',
            '.....##..................................###....',
            '.....##..................................###....',
            '.....##..................................###....',
            '...#######......####..........####.......###....',
            '...#######....########......########.....###....',
            '....###......####..####....####..####....###....',
            '....###......###....###....###....###....###....',
            '....###.....###......###..###......###...###....',
            '....###.....###......###..###......###...###....',
            '....###.....###......###..###......###...###....',
            '....###.....###......###..###......###...###....',
            '....###.....###......###..###......###...###....',
            '.....##......###....###....###....###....###....',
            '.....###.....####..####....####..####....###....',
            '.....#####....########......########.....####...',
            '......####......####..........####........###...',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        thinking: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '..........##..........#..............##.........',
            '....#.....##.........###.............##.........',
            '....##....##.........................##.........',
            '....##....##.........................##.........',
            '..######..##..###....##...##..###....##....##...',
            '...#####..########...##...########...##...##....',
            '....##....###...##...##...###...##...##..##.....',
            '....##....##....##...##...##....##...##.###.....',
            '....##....##....##...##...##....##...#####......',
            '....##....##....##...##...##....##...######.....',
            '....##....##....##...##...##....##...###..##....',
            '....##....##....##...##...##....##...##...##....',
            '....####..##....##...##...##....##...##....##...',
            '.....###..##....##...##...##....##...##....##...',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        responding: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '.................................##.............',
            '....######.......................##.............',
            '....#######......................##.............',
            '....#.....#......................##.............',
            '....#.....##....####...##.###....##..##.....#...',
            '....#....##....######..#######...##...##...##...',
            '....##.####...##...##..##....##..##...##...##...',
            '....######....##....#..##....##..##....#...#....',
            '....#...##....#######..##....##..##....##.##....',
            '....#...##....##.......##....##..##....##.##....',
            '....#....##...##.......##....##..##.....#.#.....',
            '....#....##....###.##..#######...###....###.....',
            '....#.....##....####...##.###.....##....###.....',
            '.......................##................#......',
            '.......................##...............##......',
            '.......................##.............###.......',
            '.......................##.............##........',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        delegating: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '...............................#................',
            '..........###..####.###..###..###..###..........',
            '............#..#.#..#..#.#..#..#...#............',
            '..........###..###.#####.#..#..#...##...........',
            '..........#.#..#....#....#..#..#.....#..........',
            '..........###..###..###..#..#..##..##...........',
            '..................#.............................',
            '...............###..............................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '..................#.............................',
            '..................#.............................',
            '...#..#..###...####..###..####..#..#.###.#...#..',
            '...#..#..#..#..#..#..#..#.#...#.##.#...#..#.#...',
            '...#..#..#..#.##..#.#####.#...#..#.#.###..#.#...',
            '...#..#..#..#..#..#..#....#...##.##..#.#...##...',
            '...####..#..#..####..###..#...##..#..###...#....',
            '...........................................#....',
            '..........................................#.....',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        waiting: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '..................................##............',
            '..................................##............',
            '..................................##............',
            '....##.###.....###.....###.....#####...####.....',
            '....#######...##.##...##.##...######..##..#.....',
            '....##...##..##...##.##...##.##...##..##........',
            '....##...##..##...##.##...##.##...##..###.......',
            '....##...##..#######.#######.##...##...####.....',
            '....##...##..##......##......##...##.....##.....',
            '....##...##..##......##......##...##......##....',
            '....##...##...##..#...##..#..#######..##.##.....',
            '....##...##....####....####...####.#...###......',
            '................................................',
            '................................................',
            '...........##....##...###....##...##............',
            '............#....#...#####...##...##............',
            '............##...#..##...##..##...##............',
            '............##..##..##....#..##...##............',
            '.............#..##..##....#..##...##............',
            '.............##.#...##....#..##...##............',
            '..............#.#...##...##..##...##............',
            '..............###....######...######............',
            '..............##......###.....###..#............',
            '..............##................................',
            '..............##................................',
            '.............##.................................',
            '............##..................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        done: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '....######......................................',
            '....#######.....................................',
            '....##...###....................................',
            '....##....##.....####....##..###......####......',
            '....##.....##...######...########....######.....',
            '....##.....##..###...##..###...##...##....##....',
            '....##.....##..##....##..##....##...##....##....',
            '....##.....##..##.....##.##....##...########....',
            '....##.....##..##.....##.##....##...##..........',
            '....##....##...##....##..##....##...##..........',
            '....##...###...###...##..##....##...###.........',
            '....#######.....######...##....##....######.....',
            '....######.......####....##....##.....#####.....',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
    },
    'zh-Hans': {
        reading: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '...............##...............................',
            '.....#.........##........##########.............',
            '....###....###########...##########.............',
            '......##...###########....##....##.#########....',
            '......###......##.........##....##.#########....',
            '...............##.........##....##..##....##....',
            '..........#############...########..##....##....',
            '..........############....########...#....##....',
            '...#####............##....##....##...##...##....',
            '...#####....##..##..##....##....##...##..##.....',
            '......##.....#####..#.....##....##...##..##.....',
            '......##..##..#.##........########....##.##.....',
            '......##...###..##........########....#####.....',
            '......##....##..##........##....##....####......',
            '......##..#############...##....##.....###......',
            '......##.##############...##...####....##.......',
            '......####....###........##########...####......',
            '......####...###.###.....#########...######.....',
            '......###...###...###....#......##..###..###....',
            '.....###...###.....###..........##.###....###...',
            '.....##..####........##.........##.##......##...',
            '..........#.....................#...............',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        editing: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................#...........#...................',
            '......##.......###..........##....##########....',
            '......##........##..........##....###....###....',
            '.....##....###########...########.##......##....',
            '.....##....###########...########.##########....',
            '.....##....##.......##.....##.....##########....',
            '....##..##.##.......##.....##...................',
            '....##.##..###########.....#.##..############...',
            '...######..###########....##.##..############...',
            '...#####...##.............##.##....#......##....',
            '......##...###########...########..#......##....',
            '.....##....############..########..#########....',
            '....###.#..###..#.##.##......##....#########....',
            '...######..###..#.##.##......##....#......##....',
            '...#####...###..#.##.##......##.#..#########....',
            '..........#############...#######..#########....',
            '........#.##.##########..#######...#......##....',
            '.....#######.#..#.##.##..##..##...##.....####...',
            '...####..###.#..#.##.##......##..############...',
            '...##....##..#..#.##.##......##..###########....',
            '.........##..#..#.####.......##...........##....',
            '..............................#...........##....',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        searching: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '......#........##.................##............',
            '.....##......####.................##............',
            '.....##....######.####....##################....',
            '.....##....##..##.####....##################....',
            '.....##....#...##...##............##............',
            '...######..#...##...##............##............',
            '...######..###.##.####...####################...',
            '.....##....###.##.####...####################...',
            '.....##....#...##...##...##.....###........##...',
            '.....##....###########...##...###....###...##...',
            '.....####..###########......####....###.........',
            '.....#####.....##...........#########...........',
            '...#####.......##..............####....##.......',
            '...####...############........###.......##......',
            '.....##....###########.....################.....',
            '.....##.....##.....##.....###########.....##....',
            '.....##.....###...##..............##......##....',
            '.....##......###.###.........###..##..###.......',
            '.....##.......#####.........###...##...###......',
            '.....##......#######......####....##....####....',
            '...####..######..######...###..#####......##....',
            '....###..###........##..........###.............',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        testing: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................##..............................',
            '...#######......##.........#....................',
            '...#######.....###........###...#############...',
            '........##....#####........###..#############...',
            '........##....##..##........###.......##........',
            '....##..##...##....##........#........##........',
            '....##..##.###......###...............##........',
            '....##..###############...............##........',
            '....##..##.#..######..#..#####....#...##........',
            '....##..##...............#####....##..##........',
            '....#...##..................##....##..######....',
            '....#######..#..##...##.....##....##..######....',
            '....#######.##..##..##......##....##..##........',
            '.........##..##..#..##......##....##..##........',
            '.........##..##..#..##......##....##..##........',
            '......##.##..##..####.......##....##..##........',
            '...#####.##..##....##.......####..##..##........',
            '...##....##........#........####..##..##........',
            '.........##.......##........###...##..##........',
            '........##..###########....###..#############...',
            '.....#####..###########....##...#############...',
            '......###.......................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        executing: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '.............................##.................',
            '....##....############......###...##########....',
            '....###...############.....###....##########....',
            '.....###..................###...................',
            '......##.................###....................',
            '.........................##...##................',
            '.........##############......###................',
            '...####..##############.....###..############...',
            '...#####.....##............###...############...',
            '......##....###..##.......####..........##......',
            '......##....##....##.....#####..........##......',
            '......##...###....##.....#####..........##......',
            '......##...##......##....##.##..........##......',
            '......##..############......##..........##......',
            '......##..############......##..........##......',
            '......##..##........##......##..........##......',
            '.....####...................##..........##......',
            '....#######.................##..........##......',
            '...###...##############.....##..........##......',
            '...##......############.....##......######......',
            '............................##......#####.......',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        browsing: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '..............#...#.............................',
            '.............##...##......##########............',
            '.............###########..##..##...#............',
            '............##....##......#...##...#............',
            '...........###.########...##..##...#............',
            '..........####.#..##..#...##########............',
            '............##.#..##..#...#...##...#............',
            '............##.########...#...##...#............',
            '............##.#..#.......##########............',
            '............##.##.#.......#...##...#............',
            '............##..###.......#...##...#............',
            '............##..####.....##...##...#............',
            '............##.##..####..#....##...#............',
            '............##.#.....##..#....##.###............',
            '................................................',
            '................................................',
            '................................................',
            '...............#......#..#......................',
            '....##..##.....#...#.##..#.......####..####.....',
            '.....#...#...#.#...#.##.######...#..#..#..#.....',
            '.......#####.#.#...#.##.#........#..#..#..#.....',
            '..........#..#.#...#.####..#.....####..####.....',
            '....#..#..#..#.#....................##..##......',
            '.....#.####..#.#....#########...############....',
            '........##...#.#....#......##.....##...##.......',
            '.........#...#.#....#..##..##....##.....###.....',
            '.....#..###..#.#....#..##..##...#####..#####....',
            '.....#..###..#.#....#..###.#.....#..#..#..##....',
            '....##.##.##...#......####.......#..#..#..#.....',
            '....#.##.......#...####.##...#...####..####.....',
            '.............###..###....#####...#..#..#..#.....',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        computer: [
            '................................................',
            '................................................',
            '......##........................................',
            '......##.....########........##....##...........',
            '......##.....##...###........##...##............',
            '......##.....#.....##.......##....##............',
            '......##.....##...###.......##...###########....',
            '....######...########......###...###########....',
            '....######.................##...##..##..........',
            '......##...#####.######...###...##..##..........',
            '......##...#..##.##..##..####..##...##..........',
            '......##...#..##.##..##..####..##...########....',
            '......####.#..##.##..##..#.##.......########....',
            '....######.#####.######....##.......##..........',
            '....####........#..........##.......##..........',
            '......##...############....##.......##..........',
            '......##..#############....##.......########....',
            '......##......#####........##.......########....',
            '......##.....#######.......##.......##..........',
            '......##....###.#.###......##.......##..........',
            '......##..####..#..####....##.......##..........',
            '.....###.###....#....##....##.......##..........',
            '.....##.........#...........#.......##..........',
            '................................................',
            '................................................',
            '............##.......................#..........',
            '............##............#####.....##..........',
            '............##............#####......##.........',
            '......###############.....#...#.############....',
            '.....################.....#...#.############....',
            '.....###....##.....##.....#...#.................',
            '.....###....##.....##.....#####........##.......',
            '.....###....##.....##.....#####.##.#...#..#.....',
            '.....################.....#...#.#####.##..#.....',
            '.....################.....#...#.##..####..#.....',
            '.....###....##.....##.....#...#.##..###...#.....',
            '.....###....##.....##.....#####.##...##...#.....',
            '.....###....##.....##.....#####.##...###..#.....',
            '.....################.....#...#.##..####..#.....',
            '.....################.....#...#.##.###.####.....',
            '.....###....##.......##..##...#.#####..####.....',
            '............##.......##..##...#.##.#......#.....',
            '............###......##..##...#.##........#.....',
            '.............##########..##...#.###########.....',
            '.............#########...#..###..##########.....',
            '.........................#..##............#.....',
            '................................................',
            '................................................',
        ],
        memory: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '............##....##..........##................',
            '............##....##.....############...........',
            '............##...#.##.........##................',
            '...........####.##..##........##................',
            '............##.##....##..############...........',
            '............#########.#..#...##.....#...........',
            '............###.............#...##..............',
            '...........####...#..##....#####................',
            '...........###..#.##.#......##....#.............',
            '...........###..#..#.#....##########............',
            '............##..#...#..........#....#...........',
            '............##......#......##..#.##.............',
            '............##.########..###..##..###...........',
            '............##...........#...###....#...........',
            '................................................',
            '................................................',
            '................................................',
            '.........#...........................##.........',
            '.........#........############.......##.........',
            '.........#.............##............##.........',
            '.........#.............#........############....',
            '.........#.............##.........#.....##......',
            '.........######........###.........#....##......',
            '.........#.............#####.......#....#.......',
            '.........#.............#..###.......#..##.......',
            '.........#.............#....#.......####........',
            '.........#.............#.............##.........',
            '.........#.............#............####........',
            '.........#.............#..........###..###......',
            '....############.......#........###......###....',
            '.......................#........#..........#....',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        tool: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '......##....###########.........................',
            '.......##...###########....###############......',
            '........##..#...##...##....###############......',
            '........##..#...##...##....##....##.....##......',
            '............#.######.##....##....##.....##......',
            '............#...##...##....##....##.....##......',
            '.....####...#...##...##....###############......',
            '.....####...#...##...##....###############......',
            '.......##...#.######.##....##....##.....##......',
            '.......##...#........##....##....##.....##......',
            '.......##..##.######.##....##....##.....##......',
            '.......##..##.##..##.##....###############......',
            '.......##..##.##..##.##....###############......',
            '.......##.###.##..##.##....#.....##.....##......',
            '.......######.######.##...##.....##.....##......',
            '.......#####..##.....##...##.....##.....##......',
            '.......##.##.........##..###.....##.....##......',
            '.......#..##......####...##......##..#####......',
            '...................##....#.......##..####.......',
            '................................................',
            '................................................',
            '.............................###########........',
            '............................############........',
            '......################......##........##........',
            '......################......##........##........',
            '.............##.............############........',
            '.............##.............##........##........',
            '.............##.............##........##........',
            '.............##.............############........',
            '.............##.............##........##........',
            '.............##.............##........##........',
            '.............##.............############........',
            '.............##.............##........##........',
            '.............##..........##################.....',
            '.............##..........##################.....',
            '.............##...............#......#..........',
            '.....##################.....####....####........',
            '.....##################...####........####......',
            '.........................####...........###.....',
            '..........................#..............#......',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        thinking: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '.....################............##.............',
            '.....################............##......##.....',
            '.....##.....##.....##......################.....',
            '.....##.....##.....##.......##############......',
            '.....##.....##.....##............##...###.......',
            '.....################............##..###........',
            '.....################.....###################...',
            '.....##.....##.....##.....###################...',
            '.....##.....##.....##............###............',
            '.....################..........####.............',
            '.....################.........#############.....',
            '...........#................##############......',
            '...........###...........#####.##...............',
            '.....##.##..###....##....####..##...............',
            '.....##.##...###...##.........############......',
            '....###.##.........###........###########.......',
            '....##..##.......##.##.................##.......',
            '...###..##......##..###................##.......',
            '...##...##########...#................###.......',
            '.........#########................######........',
            '..................................#####.........',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        responding: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '....##################.......##.................',
            '....##################......################....',
            '....##..............##.....#################....',
            '....##..............##....###...................',
            '....##..............##...####...................',
            '....##..............##...###.##############.....',
            '....##...########...##.......##.........###.....',
            '....##...########...##.......##.........###.....',
            '....##...##....##...##.......##############.....',
            '....##...##....##...##.......##.........###.....',
            '....##...##....##...##.......##############.....',
            '....##...##....##...##........###...............',
            '....##...########...##........###...............',
            '....##...########...##.......#############......',
            '....##..............##......##############......',
            '....##..............##....#######.....###.......',
            '....##..............##.....#...###..####........',
            '....##################..........######..........',
            '....##################........###########.......',
            '....##..............##...########....########...',
            '.........................####.............##....',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        delegating: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '.....#..............#....#........#....#........',
            '....######.#####...##.#..#..#.....#....#........',
            '....#.##...#..##..##..#..####....##....#........',
            '....#.##...#..##..######.##......#.#########....',
            '....######.#..##.......#.#...#..##....###.......',
            '......##...#..##.........#...#..##....####......',
            '.....##.##.#####..#####..#####..##...#####......',
            '....##............##..#..#.......#...#.#.##.....',
            '......########....#####..#..#....#..##.#..#.....',
            '......#......#....##..#..####....#..#..#..##....',
            '......########....#####..##......###########....',
            '......#......#....##..#..#...#...#.....#........',
            '......########....##..#..#...#...#.....#........',
            '......#......#....##.##..#####...#.....#........',
            '................................................',
            '................................................',
            '.....................#..#............##.........',
            '....................##..#............##.........',
            '....############....#..##............##.........',
            '.........##........##..########.############....',
            '.........##.......###.##.#......##...##...##....',
            '.........##.......###.#..#......##...##...##....',
            '.........##.......###....#####..##...##...##....',
            '.........##........##....#......##...##...##....',
            '.........##........##....#......############....',
            '.........##........##....#####..##...##...##....',
            '.........##........##....#...........##.........',
            '.........##........##....#...........##.........',
            '....############...##....#...........##.........',
            '...................##....#...........##.........',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        waiting: [
            '................................................',
            '................................................',
            '.......#.......#................................',
            '......##.......##............##..##.............',
            '......#######.#########.....##...##.............',
            '.....##################.....##...##.............',
            '....###.##..###..##........##...############....',
            '....##..##...##...##.......##...############....',
            '............##............###..###...##...##....',
            '......###############....####..##....##...##....',
            '......###############....####.###....##...#.....',
            '............##...........#.##..#.....##.........',
            '....###################..#.##....##..##.##......',
            '....###################....##....##..##.##......',
            '.................##........##...##...##..##.....',
            '.....#################.....##...##...##..##.....',
            '.....#################.....##...##...##..##.....',
            '........##.......##........##..##....##...##....',
            '........###......##........##..##....##...##....',
            '.........###.....##........##..#.....##...#.....',
            '..........##.....##........##........##.........',
            '..............#####........##.....####..........',
            '..............####.........##......###..........',
            '................................................',
            '................................................',
            '................##..............................',
            '.......##.......##...............##########.....',
            '.......##.......##.......#######.##########.....',
            '.......##.......##.......#######.#...##...#.....',
            '.......#######..##.........##....#...##...#.....',
            '......########..##.........##....##########.....',
            '......##....##..###........##....##########.....',
            '......##...##...####.......##....#...##...#.....',
            '.....###...##...#####....######..#...##...#.....',
            '.....###...##...##.##....######..##########.....',
            '....#####..##...##..##.....##....##########.....',
            '....##.##.##....##..###....##........##.........',
            '........####....##.........##........##.........',
            '........####....##.........##........##.........',
            '.........##.....##.........##...############....',
            '........####....##.........#################....',
            '.......######...##.......######......##.........',
            '......###..#####.........####........##.........',
            '.....###....###########........#############....',
            '....###........########........#############....',
            '.....#..........................................',
            '................................................',
            '................................................',
        ],
        done: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '............#.......................##..........',
            '...........###......................##.###......',
            '............##......................##..###.....',
            '....##################..............##....#.....',
            '....##################.....##################...',
            '....##..............##.....##################...',
            '....##..............##.....##.......##..........',
            '....##.############.##.....##.......##..........',
            '.......############........##.......##...###....',
            '...........................#######..##...##.....',
            '...........................#######..###..##.....',
            '...####################....##...##...##.##......',
            '...####################...###...##...##.##......',
            '.........##....##.........###...##...####.......',
            '.........##....##.........###...##...####.......',
            '.........##....##.........##....##...###...#....',
            '........##.....##....#....##.#####...###...##...',
            '........##.....##....##...##..###..######..##...',
            '......###......##....##..###......####.##..##...',
            '...######......########..##......####..######...',
            '...####........#######...##.......#.....####....',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
    },
    'zh-Hant': {
        reading: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................#...............................',
            '.....##........###.......##########.............',
            '.....###...############..##########.............',
            '......##........##........##....##.#########....',
            '...#######.###########....##....##.#########....',
            '...#######................##....##..##....##....',
            '...........############...########..##....##....',
            '....#####..#..##..#..##...########...#....##....',
            '....#####..#..##..#..##...##....##...##...##....',
            '...........#..##..#..##...##....##...##..##.....',
            '....#####..############...##....##...##..##.....',
            '...#######................########....##.##.....',
            '...........###########....########....#####.....',
            '...........###......##....##....##....####......',
            '....#####..###......##....##....##.....###......',
            '....#####..###########....##...####....##.......',
            '....#...#..##.......##...##########...####......',
            '....#...#..###########...#########...######.....',
            '....#...#....##...##.....#......##..###..###....',
            '....#####..####...####..........##.###....###...',
            '....#####.###.......###.........##.##......##...',
            '....#...........................#...............',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        editing: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................#............##.................',
            '......##........##...........##.....########....',
            '......##........##...........##.....##....##....',
            '.....##....###########...#########..#.....##....',
            '.....##.##.###########...#########..########....',
            '....##..##.##.......##.......##.....########....',
            '....##.##..##.......##....########..............',
            '...######..###########....#####.##.##########...',
            '...#####...###########....#..#..#############...',
            '.....####..##.............#.###.##..##....##....',
            '.....##.##.##.............########..##....##....',
            '....##..##.############...#..#..##..########....',
            '...#######.###..#..#.##...#####.##..##....##....',
            '...#######.###..#..#.##...########..##....##....',
            '...........###..#..#.##......##.....########....',
            '........#..############......##.....##....##....',
            '....#.#.###############..#########..##....##....',
            '...##.########..#..#.##..#############....###...',
            '...##.##.###.#..#..#.##......##...###########...',
            '...##.##..##.#..#..#.##......##...###.....##....',
            '...#......#..#..#...###......##...........##....',
            '.............................#............##....',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        searching: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '......#........##...............................',
            '.....##......####...........##############......',
            '.....##....######.####..................##......',
            '.....##....##..##.####..................##......',
            '.....##....#...##...##......##############......',
            '...######..#...##...##..................##......',
            '...######..###.##.####..................##......',
            '.....##....###.##.####.....###############......',
            '.....##....#...##...##..........................',
            '.....##....###########....########.#########....',
            '.....####..###########......###....###...###....',
            '.....#####.....##............##....##.....##....',
            '...#####.......##............#####.#########....',
            '...####...############...########.....###.......',
            '.....##....###########.................##.......',
            '.....##.....##.....##....####################...',
            '.....##.....###...##.....####################...',
            '.....##......###.###.........###.......##.......',
            '.....##.......#####...........###......##.......',
            '.....##......#######...........##.....###.......',
            '...####..######..######............#####........',
            '....###..###........##..............###.........',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        testing: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................##..............................',
            '....#######.....##.........##....#####.##.#.....',
            '....#######....####.........##...#####.####.....',
            '....#..##.....##..##........##......##..##......',
            '....#..##....##....###...#######.#####..##.##...',
            '....###################..#######.####...####....',
            '....#########.#########...........###....##.....',
            '....#..##.................#####...##########....',
            '....#######...............######.##.#####.###...',
            '....#######.#####.####..........###........##...',
            '....#..##...##.##.#..#....#####...#########.....',
            '....#..##...##.##.#..#....######..#########.....',
            '....#######.##.##.#.##............##.....##.....',
            '....#######.#####.####....######..##.....##.....',
            '..........#...............######..#########.....',
            '...######.#..##....##.....#...##...########.....',
            '...#.#.####..##....##.....#...##...##...##......',
            '...#.#.#.##..###..###.....#...##....##..##......',
            '...#.#...##.#####.####....######....##..##......',
            '...#.....##.##.########...######.############...',
            '.......######....##..##...#.....#############...',
            '........#.......................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        executing: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '.......##......##...............................',
            '.......##......##............##.................',
            '....########...##...........###...##########....',
            '....########...##..........###....##########....',
            '.......##......##.........###...................',
            '.......##....########....###....................',
            '...##################....##...##................',
            '...##########..##..##........###................',
            '.....#....#....##..##.......###..############...',
            '.....##..##....##..##......###...############...',
            '.....##..##..####..##.....####..........##......',
            '....########.####..##....#####..........##......',
            '....########..###..##....#####..........##......',
            '.......##......###.##....##.##..........##......',
            '.......##.....#######.......##..........##......',
            '...##########.##.####.......##..........##......',
            '...##########.##...##.......##..........##......',
            '.......##....##.....#.#.....##..........##......',
            '.......##....##.....###.....##..........##......',
            '.......##...##......###.....##..........##......',
            '.......##..###.......##.....##......######......',
            '.......##...................##......#####.......',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        browsing: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '..............#...#.............................',
            '.............##...##......##########............',
            '.............###########..##..##...#............',
            '............##....##......#...##...#............',
            '...........###.########...##..##...#............',
            '..........####.#..##..#...##########............',
            '............##.#..##..#...#...##...#............',
            '............##.########...#...##...#............',
            '............##.#..#.......##########............',
            '............##.##.#.......#...##...#............',
            '............##..###.......#...##...#............',
            '............##..####.....##...##...#............',
            '............##.##..####..#....##...#............',
            '............##.#.....##..#....##.###............',
            '................................................',
            '................................................',
            '................................................',
            '........#......#..######.#......................',
            '....#..#..###..#..#..#..######...####..####.....',
            '.....###..#.##.#..#######..#.....#..#..#..#.....',
            '.......##.#..#.#..#####..#####...#..#..#..#.....',
            '.......##.##.#.#..#..#..#.#..#...####..####.....',
            '....#....###.#.#..######.#####......##..##......',
            '.....#..#.##.#.#....########....############....',
            '.......#####.#.#....#......#......##...##.......',
            '.....#...#...#.#....########.....##.....###.....',
            '.....#.#####.#.#....########....#####..#####....',
            '.....#...#.#...#....#......#.....#..#..#..##....',
            '....#..#.#.#...#....########.....#..#..#..#.....',
            '....#..#####...#.....#...#...#...####..####.....',
            '.............###..###....#####...#..#..#..#.....',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        computer: [
            '................................................',
            '................................................',
            '......##........................................',
            '......##.....########........##....##...........',
            '......##.....##...###........##...##............',
            '......##.....#.....##.......##....##............',
            '......##.....##...###.......##...###########....',
            '....######...########......###...###########....',
            '....######.................##...##..##..........',
            '......##...#####.######...###...##..##..........',
            '......##...#..##.##..##..####..##...##..........',
            '......##...#..##.##..##..####..##...########....',
            '......####.#..##.##..##..#.##.......########....',
            '....######.#####.######....##.......##..........',
            '....####........#..........##.......##..........',
            '......##...############....##.......##..........',
            '......##..#############....##.......########....',
            '......##......#####........##.......########....',
            '......##.....#######.......##.......##..........',
            '......##....###.#.###......##.......##..........',
            '......##..####..#..####....##.......##..........',
            '.....###.###....#....##....##.......##..........',
            '.....##.........#...........#.......##..........',
            '................................................',
            '................................................',
            '................................................',
            '......###############.....#####...##.##..##.....',
            '......###############.....#####..##..##..##.....',
            '............##............##..#..#..##..##......',
            '....###################...##..#.##..##..##......',
            '....###.....###.....###...##..#..##.##..##......',
            '....##.#######.#####.##...#####..##..##..##.....',
            '....##......##.......##...#####...##..##..##....',
            '............##............##..#...##.##...#.....',
            '.......#######.#####......##..#......##.........',
            '..........................##..#..###########....',
            '......###############.....#####..###########....',
            '......###...###...###.....#####..##....##.##....',
            '......##....##.....##.....#...#..##.####..##....',
            '......###############.....#...#..##..###..##....',
            '......##....##.....##.....#...#..##.####..##....',
            '......###############....##...#..####..##.##....',
            '......###...###.......#..##...#..##.......##....',
            '.......#....##.......##..##...#..###########....',
            '.............##########..#..###..###########....',
            '...............######.......###..#..............',
            '................................................',
            '................................................',
        ],
        memory: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '............#.....##..........##................',
            '............#....####....############...........',
            '............#...##..###.......##................',
            '...........##########.#.......##................',
            '............##.#.........############...........',
            '............##.###.####..#...##.....#...........',
            '............##.#.###..#.....#...##..............',
            '...........#####.###..#....#####................',
            '...........##..########.....##....#.............',
            '...........##...#...#.....##########............',
            '............#...#...#..........#....#...........',
            '............#..###.###.....##..#.##.............',
            '............#.##..##.##..###..##..###...........',
            '............#.#..........#...###....#...........',
            '................................................',
            '................................................',
            '................................................',
            '.........#...........................##.........',
            '.........#........############.......##.........',
            '.........#.............##............##.........',
            '.........#.............#........############....',
            '.........#.............##.........#.....##......',
            '.........######........###.........#....##......',
            '.........#.............#####.......#....#.......',
            '.........#.............#..###.......#..##.......',
            '.........#.............#....#.......####........',
            '.........#.............#.............##.........',
            '.........#.............#............####........',
            '.........#.............#..........###..###......',
            '....############.......#........###......###....',
            '.......................#........#..........#....',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        tool: [
            '................................................',
            '................................................',
            '................................................',
            '.................#####...................##.....',
            '.....#####.###########....#######..##....##.....',
            '....######.#######........#######..##....##.....',
            '....##..##..#...##..##....##...##..##....##.....',
            '....##..##.##...##..##....##...##..##....##.....',
            '....##..##..##..##..##....##...##..##....##.....',
            '....##..##..##..##..#.....##...##..##....##.....',
            '....##..##..##..##.##.....##...##..##....##.....',
            '....##..##...#..##.##.....##...##..##....##.....',
            '....##..##......##........##...##..##....##.....',
            '....##..##.############...##...##..##....##.....',
            '....##..##.############...##...##..##....##.....',
            '....######......##........#######..########.....',
            '....######......##........#######.#########.....',
            '....##..........##........##......###....##.....',
            '.....#..........##........##.............##.....',
            '................##.......................##.....',
            '................##.......................##.....',
            '.............#####.......................##.....',
            '.............####........................##.....',
            '................................................',
            '................................................',
            '.............................############.......',
            '............................#############.......',
            '.....#################......##.........##.......',
            '.....#################......###.......###.......',
            '............###.............#############.......',
            '............###.............##.........##.......',
            '............###.............###.......###.......',
            '............###.............#############.......',
            '............###.............##.........##.......',
            '............###.............##.........##.......',
            '............###.............#############.......',
            '............###.............###.......###.......',
            '............###.............##.........##.......',
            '............###..........###################....',
            '............###..........###################....',
            '............###...............##.....##.........',
            '....###################.....####.....####.......',
            '....###################...####.........####.....',
            '.........................####............###....',
            '..........................#...............#.....',
            '................................................',
            '................................................',
            '................................................',
        ],
        thinking: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '.....################............##.............',
            '.....################............##......##.....',
            '.....##.....##.....##......################.....',
            '.....##.....##.....##.......##############......',
            '.....##.....##.....##............##...###.......',
            '.....################............##..###........',
            '.....################.....###################...',
            '.....##.....##.....##.....###################...',
            '.....##.....##.....##............###............',
            '.....################..........####.............',
            '.....################.........#############.....',
            '...........#................##############......',
            '...........###...........#####.##...............',
            '.....##.##..###....##....####..##...............',
            '.....##.##...###...##.........############......',
            '....###.##.........###........###########.......',
            '....##..##.......##.##.................##.......',
            '...###..##......##..###................##.......',
            '...##...##########...#................###.......',
            '.........#########................######........',
            '..................................#####.........',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        responding: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '....##################...####################...',
            '....##################.........###..###.........',
            '....##..............##.........##...###.........',
            '....##..............##....##################....',
            '....##..............##....##...##...##....##....',
            '....##..............##....##################....',
            '....##...########...##.......#...##.............',
            '....##...########...##......##...###............',
            '....##...##....##...##.....###..############....',
            '....##...##....##...##....###..###..............',
            '....##...##....##...##...###.##############.....',
            '....##...##....##...##......####.##......##.....',
            '....##...########...##.....###...##########.....',
            '....##...########...##....###....##########.....',
            '....##..............##...####.....####..........',
            '....##..............##......#....##########.....',
            '....##..............##......#..#####...###......',
            '....##################......#......######.......',
            '....##################......#.....########......',
            '....##..............##......#..#####...######...',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        delegating: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '..........#............#####...##.#####.........',
            '..........#..#.#..#####..#.#...#..#...#.........',
            '.........#...#..#...#.#..#.#...#..#...#.........',
            '........##...####...#.######..###.#####.........',
            '........##.####....####..#.#...#................',
            '.........#...#......#..#####..###.#####.........',
            '.........#...##.....#....#....###...#...........',
            '.........#....#.....#.######.#.#..#####.........',
            '.........#....#..#.###...#.....#....#...........',
            '.........#....######.....#.....#....#...........',
            '.........#.....##.....#######..#.#######........',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '........#.#................#............#.......',
            '........#..#..#########...#..#..........#.......',
            '...#########......#.......#..######.#########...',
            '........#.........#......##.#.#.....#...#...#...',
            '........#.........#......##.#.#.....#...#...#...',
            '...#######........#......##...####..#...#...#...',
            '.....#...#........#.......#...#.....#########...',
            '.....#...#........#.......#...####..#...#...#...',
            '.....#...#..#.....#.......#...#.........#.......',
            '...#####.##.#.#########...#...#.........#.......',
            '...#......##..............#...#.........#.......',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        waiting: [
            '................................................',
            '................................................',
            '.......#.......#................................',
            '......##.......##............##..##.............',
            '......#######.#########.....##...##.............',
            '.....##################.....##...##.............',
            '....###.##..###..##........##...############....',
            '....##..##...##...##.......##...############....',
            '............##............###..###...##...##....',
            '......###############....####..##....##...##....',
            '......###############....####.###....##...#.....',
            '............##...........#.##..#.....##.........',
            '....###################..#.##....##..##.##......',
            '....###################....##....##..##.##......',
            '.................##........##...##...##..##.....',
            '.....#################.....##...##...##..##.....',
            '.....#################.....##...##...##..##.....',
            '........##.......##........##..##....##...##....',
            '........###......##........##..##....##...##....',
            '.........###.....##........##..#.....##...#.....',
            '..........##.....##........##........##.........',
            '..............#####........##.....####..........',
            '..............####.........##......###..........',
            '................................................',
            '................................................',
            '............##..................................',
            '............###..................##########.....',
            '............#########....#######.##########.....',
            '............##...........#######.#...##...#.....',
            '......################.....##....#...##...#.....',
            '......#################....##....##########.....',
            '......#.....##......##.....##....##########.....',
            '......#.....#######.##.....##....#...##...#.....',
            '......#.#######..........######..#...##...#.....',
            '......#.....###.....##...######..##########.....',
            '.....##...#..#########.....##....##########.....',
            '.....##..##................##........##.........',
            '.....##..#####.#####.......##........##.........',
            '.....##.##..##.##.##.......##........##.........',
            '.....#####..##.##..#.......##...############....',
            '.....########..##..#..#....#################....',
            '.....##...###.##...####..######......##.........',
            '.....##...##..##...###...####........##.........',
            '....##...#####.................#############....',
            '....##.###..###########........#############....',
            '.......#........#######.........................',
            '................................................',
            '................................................',
        ],
        done: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '............#.......................##..........',
            '...........###......................##.###......',
            '............##......................##..###.....',
            '....##################..............##....#.....',
            '....##################.....##################...',
            '....##..............##.....##################...',
            '....##..............##.....##.......##..........',
            '....##.############.##.....##.......##..........',
            '.......############........##.......##...###....',
            '...........................#######..##...##.....',
            '...........................#######..###..##.....',
            '...####################....##...##...##.##......',
            '...####################...###...##...##.##......',
            '.........##....##.........###...##...####.......',
            '.........##....##.........###...##...####.......',
            '.........##....##.........##....##...###...#....',
            '........##.....##....#....##.#####...###...##...',
            '........##.....##....##...##..###..######..##...',
            '......###......##....##..###......####.##..##...',
            '...######......########..##......####..######...',
            '...####........#######...##.......#.....####....',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
    },
    'ja': {
        reading: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '......#.........##..............................',
            '.....##.........##........##......##............',
            '......##...############...###.....###...........',
            '......##...############....###.....###..........',
            '...#######......##..........##......##..........',
            '...#######......##..................###.........',
            '............##########..............###.........',
            '....#####...##########..............####........',
            '....######..........................####........',
            '...........############..#####.....#####........',
            '....#####..############..#####.....##..##.......',
            '....######.##........##.....##....###..##.......',
            '...........##.##..##.##.....##....##...###......',
            '....######....##..##........##...###....###.....',
            '....######....##..##........##..###......###....',
            '....#...##....##..##........##.###.......####...',
            '....#...##...###..##........##..#.........##....',
            '....#...##...##...##..#....####.................',
            '....######..###...##..#...#######...............',
            '....######.###....#####..##....##############...',
            '....#.....###.....#####...#......############...',
            '...........#....................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        editing: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '..............#.................................',
            '........................................##......',
            '.................................#########......',
            '.......##..##.##.....................##.........',
            '.......#....#..........#..............#.........',
            '.......#....##......#..#...##........##.........',
            '......##....##......#..##..#....############....',
            '......##.....#......##.....#.........##.........',
            '......#......##...........##.........##.........',
            '.....##......##..........##..........#..........',
            '.....##.......#.........##..........##..........',
            '....##........##......###..........##...........',
            '....#.........#......###..........##............',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        searching: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '......##........#.................##............',
            '......##.......###................##............',
            '......##......#####.......##################....',
            '......##.....###.###......##################....',
            '......##...####...###.............##............',
            '...###########.....####...........##............',
            '...####################..####################...',
            '......##.....#######.....####################...',
            '.....###........#........##.....###........##...',
            '.....####..###########...##...###....###...##...',
            '.....####..###########......####....###.........',
            '.....#####.##...#...##......#########...........',
            '....####.#.##...#...##.........####....##.......',
            '....#.##.#.##...#...##........###.......##......',
            '...##.##...###########.....################.....',
            '...##.##...###########....###########.....##....',
            '...#..##.......####...............##......##....',
            '......##......##.##..........###..##..###.......',
            '......##.....###..##........###...##...###......',
            '......##...####....###....####....##....####....',
            '......##..###.......###...###..#####......##....',
            '......##..#.....................###.............',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        testing: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '......##........#...........#...................',
            '......##.......###.........##...................',
            '......##......#####.........##...############...',
            '......##.....###.###........##...############...',
            '......##...####...###....########.....##........',
            '...###########.....####..########.....##........',
            '...####################...............##........',
            '......##.....#######......######...#..##........',
            '.....###........#.........######..##..##........',
            '.....####..###########............##..##........',
            '.....####..###########....######..##..#######...',
            '.....#####.##...#...##....######..##..#######...',
            '....####.#.##...#...##............##..##........',
            '....#.##.#.##...#...##............##..##........',
            '...##.##...###########....######..##..##........',
            '...##.##...###########....######..##..##........',
            '...#..##.......####.......##..##..##..##........',
            '......##......##.##.......##..##..##..##........',
            '......##.....###..##......##..##..##..##........',
            '......##...####....###....######.############...',
            '......##..###.......###...######.############...',
            '......##..#...............#.....................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        executing: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '............##..................................',
            '............##...............##.................',
            '....##################......###...##########....',
            '....##################.....###....##########....',
            '....##..............##....###...................',
            '....##......##......##...###....................',
            '....##......##......##...##...##................',
            '......###############........###................',
            '......##############........###..############...',
            '............##.............###...############...',
            '......##############......####..........##......',
            '......##############.....#####..........##......',
            '............##...........#####..........##......',
            '............##...........##.##..........##......',
            '...####################.....##..........##......',
            '....###################.....##..........##......',
            '..........######............##..........##......',
            '.........###..###...........##..........##......',
            '.......####....###..........##..........##......',
            '....#####.......#####.......##..........##......',
            '....####..........#####.....##......######......',
            '....#................#......##......#####.......',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        browsing: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '...........#....................................',
            '.....######...####............#..#..............',
            '..........#..........######.#######.............',
            '.........#...######..#....#...#..#...######.....',
            '.........#........#..#........#..#..............',
            '........#........#.......#......................',
            '.......#........#.......#.......#...............',
            '......#........#.......#.......#................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '...........#.....#..###...#.#...................',
            '.........#####...#..###..#..####.######.........',
            '..........#......#.#..#..#.#..........#.........',
            '..........#####..#..##..##...##.......#.........',
            '.........#.##....#.####..#.......######.........',
            '..........#.#....#.####..#...##.................',
            '..........#......#..##...#......................',
            '...........###...#.#.#...#......................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        computer: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '...................................#............',
            '....####..#.....#..#...............###..........',
            '.......#........##.....##...####..#..#..####....',
            '.......#.....#..#...................#...........',
            '.......#...##...#...................##..........',
            '....####..#.....####..####........#.............',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '.............###.....##..#.#....................',
            '...................#.....####..####.............',
            '.............####..#######.##...................',
            '..............##..##.#...#.#...####.............',
            '....................###..#.##...................',
            '..............###...#....#.#....................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        memory: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '...................###..........................',
            '....#####..#....................#####...#.......',
            '........#...#.....#####...###......#....#.......',
            '........#......#....#......#.......#....###.....',
            '........#.....#.....#.....####....##....#..#....',
            '....#####...##.....#.......#.....#..#...#.......',
            '...................#............#.......#.......',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '..................#..#...#####....#.............',
            '...........####...#####....#....#####...........',
            '............#..#..#..#..#######...#.............',
            '...........#.##...#####...#.#.....#.............',
            '............##...##.###...####..#####...........',
            '............#.....#..#...###......#.............',
            '............####..#.#.#....#......#.............',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        tool: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '........##....#....................#..##........',
            '....##...#....##...................#..##........',
            '.....#...##...#....................#..##........',
            '.....##..##..##....................#..##........',
            '......#......#....############....##..##........',
            '............##.....##########.....##..##........',
            '...........##.....................##..##...#....',
            '..........###.....................#...##..##....',
            '.........###.....................##...#####.....',
            '.......###......................##....###.......',
            '.......##.......................##.....#........',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        thinking: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '.....################............##.............',
            '.....################............##......##.....',
            '.....##.....##.....##......################.....',
            '.....##.....##.....##.......##############......',
            '.....##.....##.....##............##...###.......',
            '.....################............##..###........',
            '.....################.....###################...',
            '.....##.....##.....##.....###################...',
            '.....##.....##.....##............###............',
            '.....################..........####.............',
            '.....################.........#############.....',
            '...........#................##############......',
            '...........###...........#####.##...............',
            '.....##.##..###....##....####..##...............',
            '.....##.##...###...##.........############......',
            '....###.##.........###........###########.......',
            '....##..##.......##.##.................##.......',
            '...###..##......##..###................##.......',
            '...##...##########...#................###.......',
            '.........#########................######........',
            '..................................#####.........',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        responding: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '.....................................#..........',
            '....#...........######.......##.....##..........',
            '....##....############.......##......##.........',
            '.....##...######............##.......##.........',
            '.....###..##................##.##############...',
            '......#...##...............###.##############...',
            '..........############.....###..................',
            '..........############....####...##########.....',
            '..........##.......##....#####..###########.....',
            '...#####..##.......##....#####..................',
            '...#####..##.###..###....##.##..................',
            '......##..##..###.##........##..###########.....',
            '......##..##...####.........##...##########.....',
            '......##..##....###.........##..................',
            '......##.##....#####........##..############....',
            '......##.##..####.###.......##..############....',
            '......####..###....###......##..##........##....',
            '.....###....#.......#.......##..##........##....',
            '...########.................##..##........##....',
            '...##...###############.....##..############....',
            '...##......############.....##..############....',
            '............................#...##..............',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        delegating: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '.............................#..................',
            '...................#.#...#####..................',
            '..................#####......#..................',
            '...................#.#......#...................',
            '.....................#......#...................',
            '.....................#.....#....................',
            '....................#.....#.....................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '....#####.......................#.......#.......',
            '......#...........#...#...###....#......#.......',
            '......#....#####......#....#........#...###.....',
            '......#.............#......#.......#....#..#....',
            '....#####.........##.......#.....##.....#.......',
            '.........................#####..........#.......',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '..............#####..#.###....#.................',
            '....................#.......#####...............',
            '...............####..#.###....#.................',
            '...............###..##...#....#.................',
            '..............#####..#...#..#####...............',
            '...............#.#...#...#....#.................',
            '..............#...#..#..##....#.................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
        waiting: [
            '................................................',
            '................................................',
            '............................#.......##..........',
            '.............##............###......##..........',
            '.............###...........#######.#########....',
            '.....##################...########.#########....',
            '.....##################...##.##...###.###.......',
            '.....##..................##...##..##...##.......',
            '.....##.....###...........#......###............',
            '.....##......####...............####............',
            '.....##........###.............###.###..........',
            '.....##.....##..##...........####...####........',
            '.....##.....##.............################.....',
            '.....##..#..##.....##....##############.####....',
            '.....##..##.##.....##....###..............#.....',
            '.....##..#..##......##..........................',
            '.....##.##..##......##......#############.......',
            '.....##.##..##....##.##.....#############.......',
            '.....##.##..##....##.##.....##.........##.......',
            '....##.##...##....##.##.....##.........##.......',
            '....##......#######.........#############.......',
            '....##.......######.........#############.......',
            '............................##.........##.......',
            '................................................',
            '................................................',
            '................#...............................',
            '.......###......##.............##...............',
            '......###.......##.............##...............',
            '.....###...###########.........##...............',
            '....###....###########.........##....####.......',
            '....##..##......##........###############.......',
            '........##......##........###########...........',
            '.......##.#############.......##................',
            '......###.#############.......##................',
            '.....####.........##.........##...#####.........',
            '....#####.........##.........###########........',
            '....##.##.#############......#####....###.......',
            '.......##.#############.....####.......###......',
            '.......##.........##........###.........##......',
            '.......##...##....##.........#..........##......',
            '.......##...###...##...................###......',
            '.......##....##...##...................##.......',
            '.......##....##...##.................####.......',
            '.......##.........##...........#########........',
            '.......##.......####...........#######..........',
            '.......##.......###.............................',
            '................................................',
            '................................................',
        ],
        done: [
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '............#...................................',
            '...........###..................................',
            '............##............#################.....',
            '....##################....#################.....',
            '....##################.................####.....',
            '....##..............##................###.......',
            '....##..............##...............###........',
            '....##.############.##.............####.........',
            '.......############...............####..........',
            '..................................##............',
            '..................................##............',
            '...####################...........##............',
            '...####################...........##............',
            '.........##....##.................##............',
            '.........##....##.................##............',
            '.........##....##.................##............',
            '........##.....##....#............##............',
            '........##.....##....##...........##............',
            '......###......##....##...........##............',
            '...######......########...........##............',
            '...####........#######........######............',
            '..............................#####.............',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
            '................................................',
        ],
    },
};

};
factories["dot-body"]=function(exports,require){
"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.DOT_BODY = void 0;
exports.DOT_BODY = [
    [-0.104699, -0.388947],
    [0.101025, 0.375171],
    [-0.424305, 0.103321],
    [0.424305, -0.050972],
    [0.042247, -0.006888],
    [-0.376548, -0.212612],
    [-0.207561, 0.345781],
    [0.189192, -0.256696],
    [0.339812, 0.220878],
    [-0.178172, 0.095974],
    [-0.001837, 0.198836],
    [0.240623, 0.037196],
    [-0.001837, -0.227306],
    [-0.314096, -0.043624],
    [-0.196540, -0.234654],
    [-0.343485, 0.250267],
    [0.174498, 0.228225],
    [-0.056941, 0.382518],
    [0.049594, -0.374252],
    [0.409611, 0.095974],
    [-0.134088, 0.228225],
    [0.108372, 0.110668],
    [-0.053268, 0.081279],
    [-0.299402, 0.132710],
    [-0.435326, -0.021583],
    [-0.292054, -0.308127],
    [0.299402, -0.065666],
    [0.156130, -0.043624],
    [-0.281033, -0.153834],
    [0.258992, 0.147405],
    [0.255318, 0.301698],
    [0.101025, -0.197917],
    [-0.240623, 0.220878],
    [-0.093678, -0.278737],
    [0.020205, 0.301698],
    [-0.200213, -0.359558],
    [-0.409611, -0.117097],
    [0.343485, 0.022501],
    [-0.350833, 0.044543],
    [0.082657, 0.235572],
    [0.071636, -0.286085],
    [-0.071636, 0.294350],
    [0.170824, 0.323740],
    [-0.031226, -0.344863],
    [-0.255318, 0.059237],
    [0.023879, 0.118016],
    [-0.108372, 0.147405],
    [-0.387569, 0.176794],
    [0.119393, 0.029848],
    [0.192866, 0.103321],
    [-0.277360, 0.294350],
    [0.332464, 0.118016],
    [-0.130414, 0.353129],
    [0.023879, 0.382518],
    [-0.277360, -0.234654],
    [0.431653, 0.022501],
    [-0.185519, 0.169447],
    [0.258992, 0.220878],
    [0.229603, -0.043624],
    [-0.119393, 0.051890],
    [-0.339812, -0.109750],
    [-0.192866, 0.272309],
    [-0.163477, -0.300779],
    [0.126741, 0.176794],
    [0.383895, 0.162099],
    [-0.067962, 0.213530],
    [-0.020205, 0.015154],
    [0.053268, 0.059237],
    [-0.380222, -0.058319],
    [-0.042247, 0.147405],
    [-0.303075, 0.198836],
    [-0.229603, -0.300779],
    [0.009184, -0.293432],
    [0.361854, -0.058319],
    [-0.361854, 0.110668],
    [-0.236950, 0.125363],
    [0.064289, 0.169447],
    [-0.413285, 0.037196],
    [-0.336138, -0.264043],
    [0.130414, -0.271390],
    [0.284707, -0.006888],
    [0.178172, 0.015154],
    [-0.023879, 0.257614],
    [-0.137762, 0.294350],
    [0.108372, 0.316392],
    [-0.339812, -0.168528],
    [-0.145109, -0.352210],
    [-0.086331, -0.337516],
    [0.053268, -0.234654],
    [0.101025, -0.021583],
    [0.299402, 0.257614],
    [0.214908, 0.264961],
    [-0.031226, 0.331087],
    [-0.042247, -0.264043],
    [0.152456, -0.212612],
    [-0.001837, 0.066585],
    [0.387569, -0.006888],
    [-0.306749, 0.015154],
    [-0.306749, 0.073932],
    [0.148782, 0.073932],
    [0.303075, 0.176794],
    [0.027552, 0.242919],
    [-0.369201, -0.006888],
    [0.295728, 0.044543],
    [0.130414, 0.257614],
    [-0.189192, 0.220878],
    [-0.244297, -0.197917],
    [0.159803, 0.140058],
    [0.225929, 0.184141],
    [-0.053268, -0.388947],
    [-0.141435, -0.256696],
    [-0.398590, -0.168528],
    [-0.431653, -0.073014],
    [-0.258992, 0.169447],
    [0.001837, -0.381599],
    [-0.130414, 0.103321],
    [-0.336138, 0.162099],
    [-0.229603, 0.301698],
    [-0.321444, -0.219959],
    [0.328791, -0.021583],
    [-0.075310, 0.037196],
    [-0.288381, 0.242919],
    [-0.086331, 0.338434],
    [0.060615, 0.279656],
    [0.023879, -0.337516],
    [-0.387569, 0.073932],
    [-0.093678, 0.250267],
    [0.145109, 0.360476],
    [-0.236950, -0.256696],
    [0.200213, 0.059237],
    [-0.141435, 0.184141],
    [-0.350833, 0.206183],
    [0.240623, 0.081279],
    [0.214908, 0.323740],
    [-0.056941, -0.308127],
    [0.101025, -0.242001],
    [-0.369201, -0.139139],
    [-0.207561, 0.066585],
    [-0.082657, 0.110668],
    [-0.156130, 0.140058],
    [-0.101025, 0.382518],
    [-0.247971, -0.337516],
    [-0.123067, -0.308127],
    [-0.299402, -0.117097],
    [0.222255, 0.000459],
    [0.071636, 0.022501],
    [0.023879, 0.029848],
    [0.093678, 0.066585],
    [0.067962, 0.103321],
    [-0.023879, 0.110668],
    [-0.405937, 0.140058],
    [-0.001837, 0.154752],
    [0.045921, 0.206183],
    [-0.247971, 0.264961],
    [-0.016531, 0.375171],
    [-0.196540, -0.278737],
    [-0.273686, 0.095974],
    [-0.075310, 0.169447],
    [-0.163477, 0.331087],
    [0.056941, 0.360476],
    [0.192866, -0.028930],
    [0.137762, -0.006888],
    [0.347159, 0.176794],
    [-0.222255, 0.184141],
    [-0.104699, 0.198836],
    [0.178172, 0.279656],
    [-0.288381, -0.197917],
    [0.042247, -0.197917],
    [0.420632, 0.059237],
    [-0.196540, 0.132710],
    [0.097351, 0.147405],
    [0.093678, 0.198836],
    [-0.196540, -0.322821],
    [-0.295728, -0.271390],
    [-0.376548, -0.095055],
    [-0.310423, -0.080361],
    [0.427979, -0.014235],
    [0.380222, 0.125363],
    [-0.038573, 0.184141],
    [-0.027552, 0.294350],
    [0.031226, -0.264043],
    [0.262665, -0.065666],
    [-0.156130, 0.066585],
    [-0.156130, 0.257614],
    [-0.310423, 0.272309],
    [-0.023879, -0.300779],
    [-0.262665, -0.278737],
    [0.156130, -0.249348],
    [-0.343485, -0.073014],
    [-0.347159, -0.036277],
    [-0.402264, -0.028930],
    [0.152456, 0.037196],
    [-0.042247, 0.044543],
    [-0.093678, 0.073932],
    [-0.339812, 0.081279],
    [-0.233276, 0.088627],
    [0.031226, 0.162099],
    [0.200213, 0.206183],
    [-0.034900, 0.220878],
    [-0.214908, 0.242919],
    [-0.060615, 0.257614],
    [0.266339, 0.264961],
    [-0.104699, 0.301698],
    [0.145109, 0.301698],
    [0.001837, 0.338434],
    [-0.005510, -0.264043],
    [-0.350833, -0.234654],
    [-0.314096, -0.146486],
    [0.391243, -0.043624],
    [0.258992, -0.028930],
    [-0.332464, -0.006888],
    [-0.398590, 0.007806],
    [0.255318, 0.007806],
    [0.314096, 0.007806],
    [-0.292054, 0.044543],
    [0.266339, 0.059237],
    [-0.427979, 0.066585],
    [0.027552, 0.081279],
    [0.347159, 0.088627],
    [-0.325117, 0.110668],
    [0.145109, 0.110668],
    [-0.372874, 0.147405],
    [0.317770, 0.147405],
    [-0.270013, 0.206183],
    [-0.317770, 0.228225],
    [-0.123067, 0.264961],
    [0.005510, 0.272309],
    [-0.178172, 0.301698],
    [-0.178172, 0.360476],
    [-0.137762, -0.381599],
    [-0.078983, -0.366905],
    [-0.112046, -0.359558],
    [-0.229603, -0.227306],
    [-0.347159, -0.197917],
    [0.292054, -0.036277],
    [0.380222, 0.022501],
    [0.207561, 0.029848],
    [-0.266339, 0.140058],
    [-0.229603, 0.154752],
    [-0.292054, 0.162099],
    [0.306749, 0.228225],
    [0.222255, 0.294350],
    [0.115720, 0.345781],
    [-0.262665, -0.308127],
    [-0.093678, -0.308127],
    [0.038573, -0.293432],
    [0.101025, -0.286085],
    [-0.170824, -0.256696],
    [0.071636, -0.197917],
    [-0.369201, -0.168528],
    [0.328791, -0.065666],
    [0.361854, -0.028930],
    [0.071636, -0.006888],
    [-0.435326, 0.007806],
    [-0.383895, 0.037196],
    [-0.321444, 0.044543],
    [-0.394916, 0.103321],
    [-0.053268, 0.110668],
    [-0.075310, 0.140058],
    [0.130414, 0.140058],
    [-0.001837, 0.228225],
    [-0.130414, 0.323740],
    [-0.056941, 0.353129],
    [-0.163477, -0.330168],
    [-0.005510, -0.322821],
    [-0.123067, -0.278737],
    [0.071636, -0.256696],
    [0.119393, -0.219959],
    [-0.306749, -0.175876],
    [-0.262665, -0.175876],
    [0.101025, 0.007806],
    [-0.350833, 0.015154],
    [-0.343485, 0.132710],
    [-0.170824, 0.198836],
    [0.277360, 0.198836],
    [0.233276, 0.242919],
    [-0.023879, -0.388947],
    [-0.163477, -0.374252],
    [-0.005510, -0.352210],
    [-0.060615, -0.344863],
    [0.049594, -0.344863],
    [-0.222255, -0.330168],
    [-0.067962, -0.271390],
    [-0.303075, -0.242001],
    [0.023879, -0.234654],
    [-0.339812, -0.139139],
    [-0.402264, -0.087708],
    [0.130414, -0.036277],
    [0.163477, -0.014235],
    [0.361854, 0.000459],
    [0.005510, 0.007806],
    [0.405937, 0.015154],
    [-0.001837, 0.037196],
    [0.119393, 0.059237],
    [0.174498, 0.066585],
    [-0.027552, 0.073932],
    [-0.207561, 0.095974],
    [-0.299402, 0.103321],
    [0.049594, 0.125363],
    [0.119393, 0.206183],
    [-0.214908, 0.213530],
    [-0.163477, 0.228225],
    [0.053268, 0.235572],
    [-0.222255, 0.272309],
    [0.034900, 0.272309],
    [-0.056941, 0.323740],
    [0.082657, 0.323740],
    [0.082657, 0.353129],
    [0.023879, -0.366905],
    [0.078983, -0.227306],
    [-0.409611, -0.058319],
    [0.277360, 0.022501],
    [0.093678, 0.037196],
    [0.321444, 0.037196],
    [0.126741, 0.088627],
    [0.214908, 0.088627],
    [-0.001837, 0.095974],
    [0.167151, 0.095974],
    [-0.156130, 0.110668],
    [-0.108372, 0.118016],
    [-0.001837, 0.125363],
    [-0.130414, 0.132710],
    [-0.358180, 0.176794],
    [-0.328791, 0.191488],
    [0.023879, 0.191488],
    [-0.262665, 0.235572],
    [0.152456, 0.242919],
    [-0.005510, 0.309045],
    [0.192866, 0.338434],
    [-0.152456, 0.367823],
    [-0.078983, 0.367823],
    [-0.042247, -0.366905],
    [-0.174498, -0.352210],
    [-0.123067, -0.337516],
    [-0.152456, -0.278737],
    [-0.207561, -0.256696],
    [0.130414, -0.242001],
    [0.178172, -0.234654],
    [-0.255318, -0.219959],
    [-0.387569, -0.190570],
    [-0.409611, -0.146486],
    [0.203887, -0.050972],
    [0.317770, -0.043624],
    [-0.064289, 0.059237],
    [0.229603, 0.059237],
    [-0.361854, 0.066585],
    [-0.281033, 0.066585],
    [-0.130414, 0.073932],
    [0.398590, 0.073932],
    [-0.383895, 0.125363],
    [0.078983, 0.125363],
    [0.181845, 0.125363],
    [0.270013, 0.125363],
    [-0.130414, 0.162099],
    [0.148782, 0.162099],
    [-0.012858, 0.176794],
    [-0.281033, 0.184141],
    [-0.247971, 0.191488],
    [-0.196540, 0.191488],
    [-0.376548, 0.198836],
    [0.328791, 0.198836],
    [0.358180, 0.198836],
    [0.071636, 0.213530],
    [-0.093678, 0.220878],
    [-0.361854, 0.228225],
    [-0.181845, 0.250267],
    [0.071636, 0.257614],
    [-0.277360, 0.264961],
    [-0.167151, 0.279656],
    [-0.049594, 0.279656],
    [0.244297, 0.279656],
    [-0.203887, 0.294350],
    [0.067962, 0.382518],
    [-0.034900, -0.322821],
    [-0.200213, -0.300779],
    [-0.045921, -0.286085],
    [-0.233276, -0.278737],
    [0.097351, -0.264043],
    [-0.273686, -0.256696],
    [0.020205, -0.212612],
    [-0.317770, -0.197917],
    [-0.380222, -0.117097],
    [0.387569, -0.065666],
    [-0.435326, -0.050972],
    [-0.376548, -0.036277],
    [-0.310423, -0.021583],
    [0.218582, -0.021583],
    [0.189192, -0.006888],
    [0.045921, 0.015154],
    [0.141435, 0.015154],
    [0.049594, 0.037196],
    [0.181845, 0.037196],
    [0.409611, 0.037196],
    [0.020205, 0.051890],
    [-0.181845, 0.073932],
    [0.056941, 0.081279],
    [-0.365527, 0.088627],
    [-0.152456, 0.088627],
    [0.090004, 0.088627],
    [-0.104699, 0.095974],
    [-0.277360, 0.118016],
    [0.405937, 0.118016],
    [0.020205, 0.140058],
    [-0.159803, 0.162099],
    [-0.104699, 0.169447],
    [0.093678, 0.169447],
    [-0.078983, 0.191488],
    [0.067962, 0.191488],
    [-0.145109, 0.206183],
    [0.020205, 0.213530],
    [-0.292054, 0.220878],
    [-0.071636, 0.235572],
    [-0.314096, 0.250267],
    [0.001837, 0.250267],
    [-0.097351, 0.272309],
    [-0.251644, 0.287003],
    [-0.082657, 0.316392],
    [0.141435, 0.338434],
    [0.167151, 0.345781],
    [-0.027552, 0.353129],
    [-0.104699, 0.360476],
    [0.027552, 0.360476],
    [-0.126741, 0.375171],
    [-0.082657, -0.388947],
    [-0.174498, -0.278737],
    [-0.325117, -0.242001],
    [-0.299402, -0.219959],
    [-0.266339, -0.197917],
    [0.123067, -0.197917],
    [-0.284707, -0.175876],
    [0.339812, -0.043624],
    [0.339812, 0.000459],
    [-0.372874, 0.015154],
    [-0.328791, 0.015154],
    [-0.435326, 0.037196],
    [-0.097351, 0.051890],
    [-0.229603, 0.066585],
    [0.045921, 0.103321],
    [-0.321444, 0.132710],
    [-0.207561, 0.154752],
    [-0.314096, 0.162099],
    [-0.339812, 0.228225],
    [-0.236950, 0.242919],
    [0.042247, 0.301698],
    [-0.108372, 0.323740],
    [0.045921, 0.382518],
    [-0.020205, -0.366905],
    [-0.137762, -0.322821],
    [-0.108372, -0.322821],
    [-0.071636, -0.322821],
    [-0.306749, -0.293432],
    [-0.277360, -0.293432],
    [-0.137762, -0.293432],
    [-0.108372, -0.293432],
    [-0.078983, -0.293432],
    [-0.321444, -0.278737],
    [-0.020205, -0.278737],
    [0.056941, -0.271390],
    [-0.108372, -0.264043],
    [-0.027552, -0.249348],
    [0.009184, -0.249348],
    [-0.251644, -0.242001],
    [0.056941, -0.212612],
    [-0.383895, -0.153834],
    [-0.354506, -0.153834],
    [-0.394916, -0.131792],
    [-0.354506, -0.124445],
    [-0.325117, -0.124445],
    [-0.424305, -0.102403],
    [-0.314096, -0.102403],
    [-0.354506, -0.095055],
    [-0.365527, -0.073014],
    [-0.328791, -0.058319],
    [0.277360, -0.050972],
    [0.178172, -0.043624],
    [0.405937, -0.028930],
    [0.306749, -0.021583],
    [0.244297, -0.014235],
    [-0.420632, -0.006888],
    [0.115720, -0.006888],
    [0.299402, 0.022501],
    [0.262665, 0.037196],
    [-0.398590, 0.051890],
    [-0.016531, 0.051890],
    [0.078983, 0.051890],
    [-0.336138, 0.059237],
    [-0.413285, 0.081279],
    [-0.258992, 0.081279],
    [0.189192, 0.081279],
    [-0.078983, 0.088627],
    [-0.038573, 0.095974],
    [-0.247971, 0.103321],
    [-0.222255, 0.110668],
    [-0.192866, 0.110668],
    [-0.409611, 0.118016],
    [-0.170824, 0.125363],
    [-0.038573, 0.125363],
    [-0.016531, 0.140058],
    [0.394916, 0.140058],
    [-0.181845, 0.147405],
    [0.369201, 0.147405],
    [-0.402264, 0.162099],
    [-0.027552, 0.162099],
    [0.244297, 0.162099],
    [-0.053268, 0.169447],
    [0.009184, 0.176794],
    [0.369201, 0.176794],
    [-0.119393, 0.184141],
    [-0.053268, 0.198836],
    [-0.023879, 0.198836],
    [-0.119393, 0.213530],
    [0.097351, 0.220878],
    [-0.108372, 0.235572],
    [-0.049594, 0.235572],
    [0.321444, 0.242919],
    [0.049594, 0.257614],
    [0.281033, 0.279656],
    [-0.042247, 0.309045],
    [0.200213, 0.309045],
    [0.016531, 0.323740],
    [-0.042247, 0.367823],
    [-0.031226, 0.389865],
    [-0.001837, 0.389865],
    [-0.218582, -0.352210],
    [-0.244297, -0.315474],
    [-0.178172, -0.315474],
    [0.012858, -0.315474],
    [-0.247971, -0.293432],
    [-0.214908, -0.286085],
    [0.012858, -0.271390],
    [0.170824, -0.264043],
    [0.115720, -0.256696],
    [0.038573, -0.219959],
    [0.097351, -0.219959],
    [-0.365527, -0.190570],
    [-0.295728, -0.139139],
    [-0.328791, -0.087708],
    [0.244297, -0.058319],
    [0.405937, -0.058319],
    [-0.361854, -0.050972],
    [-0.420632, -0.036277],
    [-0.328791, -0.028930],
    [-0.387569, -0.014235],
    [-0.350833, -0.014235],
    [0.409611, -0.006888],
    [0.023879, 0.000459],
    [0.159803, 0.007806],
    [0.203887, 0.007806],
    [-0.416958, 0.015154],
    [0.236950, 0.015154],
    [-0.038573, 0.022501],
    [-0.056941, 0.029848],
    [0.134088, 0.044543],
    [0.075310, 0.073932],
    [0.108372, 0.081279],
    [-0.321444, 0.088627],
    [-0.343485, 0.103321],
    [0.391243, 0.103321],
    [0.126741, 0.118016],
    [0.163477, 0.118016],
    [-0.218582, 0.132710],
    [-0.093678, 0.132710],
    [-0.056941, 0.132710],
    [0.112046, 0.132710],
    [-0.247971, 0.147405],
    [-0.354506, 0.154752],
    [-0.090004, 0.154752],
    [0.115720, 0.154752],
    [0.045921, 0.176794],
    [0.108372, 0.184141],
    [-0.020205, 0.235572],
    [-0.137762, 0.250267],
    [0.281033, 0.250267],
    [-0.328791, 0.264961],
    [-0.141435, 0.272309],
    [0.196540, 0.272309],
    [-0.292054, 0.279656],
    [-0.012858, 0.279656],
    [-0.119393, 0.287003],
    [0.163477, 0.294350],
    [0.126741, 0.309045],
    [0.236950, 0.309045],
    [-0.181845, 0.338434],
    [-0.145109, 0.338434],
    [0.001837, 0.367823],
    [0.119393, 0.367823],
    [-0.119393, -0.374252],
    [-0.097351, -0.374252],
    [-0.060615, -0.374252],
    [-0.181845, -0.366905],
    [0.042247, -0.359558],
    [-0.093678, -0.352210],
    [0.016531, -0.352210],
    [-0.192866, -0.344863],
    [-0.012858, -0.337516],
    [-0.053268, -0.330168],
    [-0.270013, -0.322821],
    [-0.214908, -0.315474],
    [-0.156130, -0.315474],
    [-0.181845, -0.293432],
    [0.078983, -0.271390],
    [-0.314096, -0.264043],
    [-0.255318, -0.264043],
    [-0.189192, -0.264043],
    [-0.086331, -0.264043],
    [0.148782, -0.264043],
    [0.049594, -0.256696],
    [-0.343485, -0.249348],
    [-0.222255, -0.242001],
    [-0.009184, -0.242001],
    [0.148782, -0.234654],
    [-0.358180, -0.219959],
    [-0.339812, -0.212612],
    [-0.281033, -0.212612],
    [-0.236950, -0.212612],
    [-0.332464, -0.183223],
    [-0.321444, -0.161181],
    [-0.299402, -0.161181],
    [-0.361854, -0.109750],
    [-0.394916, -0.102403],
    [-0.383895, -0.080361],
    [0.222255, -0.058319],
    [-0.394916, -0.043624],
    [0.251644, -0.043624],
    [0.369201, -0.043624],
    [0.211234, -0.036277],
    [0.431653, -0.036277],
    [0.148782, -0.028930],
    [0.170824, -0.028930],
    [0.236950, -0.028930],
    [0.383895, -0.028930],
    [0.123067, -0.021583],
    [0.277360, -0.021583],
    [0.347159, -0.014235],
    [0.369201, -0.014235],
    [0.093678, -0.006888],
    [0.262665, -0.006888],
    [0.321444, -0.006888],
    [-0.314096, 0.000459],
    [0.435326, 0.000459],
    [0.064289, 0.007806],
    [0.123067, 0.007806],
    [0.292054, 0.007806],
    [-0.391243, 0.022501],
    [-0.365527, 0.029848],
    [-0.343485, 0.029848],
    [-0.314096, 0.029848],
    [-0.023879, 0.037196],
    [0.112046, 0.044543],
    [0.214908, 0.044543],
    [0.427979, 0.044543],
    [-0.420632, 0.051890],
    [-0.376548, 0.051890],
    [-0.273686, 0.051890],
    [0.159803, 0.051890],
    [0.247971, 0.051890],
    [-0.314096, 0.059237],
    [-0.137762, 0.059237],
    [-0.034900, 0.059237],
    [0.141435, 0.059237],
    [-0.405937, 0.066585],
    [-0.112046, 0.066585],
    [0.034900, 0.066585],
    [-0.071636, 0.073932],
    [0.207561, 0.073932],
    [-0.288381, 0.081279],
    [-0.214908, 0.081279],
    [-0.009184, 0.081279],
    [0.416958, 0.081279],
    [-0.123067, 0.088627],
    [-0.060615, 0.095974],
    [0.020205, 0.095974],
    [0.339812, 0.103321],
    [0.005510, 0.110668],
    [0.086331, 0.110668],
    [-0.306749, 0.118016],
    [-0.255318, 0.118016],
    [-0.137762, 0.118016],
    [-0.365527, 0.132710],
    [0.325117, 0.132710],
    [0.042247, 0.140058],
    [-0.328791, 0.147405],
    [-0.306749, 0.147405],
    [-0.284707, 0.147405],
    [-0.137762, 0.147405],
    [-0.067962, 0.154752],
    [-0.380222, 0.162099],
    [0.310423, 0.162099],
    [0.354506, 0.162099],
    [-0.236950, 0.169447],
    [-0.214908, 0.169447],
    [-0.321444, 0.176794],
    [-0.299402, 0.176794],
    [-0.167151, 0.176794],
    [-0.097351, 0.184141],
    [-0.060615, 0.184141],
    [0.086331, 0.184141],
    [-0.229603, 0.198836],
    [-0.126741, 0.198836],
    [0.218582, 0.198836],
    [-0.321444, 0.206183],
    [-0.086331, 0.206183],
    [-0.369201, 0.213530],
    [-0.016531, 0.213530],
    [0.321444, 0.213530],
    [0.038573, 0.220878],
    [0.192866, 0.220878],
    [-0.222255, 0.228225],
    [-0.196540, 0.235572],
    [0.251644, 0.235572],
    [-0.270013, 0.250267],
    [-0.115720, 0.250267],
    [-0.042247, 0.250267],
    [-0.295728, 0.257614],
    [-0.229603, 0.257614],
    [-0.207561, 0.257614],
    [0.020205, 0.257614],
    [-0.174498, 0.264961],
    [-0.067962, 0.272309],
    [-0.031226, 0.272309],
    [-0.270013, 0.279656],
    [-0.185519, 0.287003],
    [0.012858, 0.287003],
    [0.262665, 0.287003],
    [-0.159803, 0.294350],
    [-0.064289, 0.309045],
    [-0.023879, 0.316392],
    [-0.123067, 0.338434],
    [-0.064289, 0.338434],
    [-0.159803, 0.353129],
    [0.075310, 0.367823],
    [-0.067962, -0.388947],
    [-0.038573, -0.388947],
    [0.016531, -0.381599],
    [-0.145109, -0.366905],
    [-0.005510, -0.366905],
    [0.009184, -0.366905],
    [-0.126741, -0.359558],
    [-0.060615, -0.359558],
    [0.056941, -0.359558],
    [-0.159803, -0.352210],
    [-0.078983, -0.352210],
    [-0.045921, -0.344863],
    [-0.145109, -0.337516],
    [-0.108372, -0.337516],
    [0.001837, -0.337516],
    [-0.178172, -0.330168],
    [-0.123067, -0.322821],
    [-0.093678, -0.322821],
    [-0.020205, -0.322821],
    [-0.229603, -0.315474],
    [-0.277360, -0.308127],
    [-0.137762, -0.308127],
    [-0.108372, -0.308127],
    [-0.078983, -0.308127],
    [-0.042247, -0.308127],
    [-0.005510, -0.308127],
    [-0.214908, -0.300779],
    [-0.292054, -0.293432],
    [-0.262665, -0.293432],
    [-0.123067, -0.293432],
    [-0.093678, -0.293432],
    [-0.064289, -0.293432],
    [-0.005510, -0.293432],
    [0.023879, -0.293432],
    [0.053268, -0.293432],
    [0.086331, -0.286085],
    [-0.277360, -0.278737],
    [-0.247971, -0.278737],
    [-0.137762, -0.278737],
    [-0.108372, -0.278737],
    [-0.005510, -0.278737],
    [0.031226, -0.278737],
    [-0.214908, -0.271390],
    [0.115720, -0.271390],
    [-0.123067, -0.264043],
    [-0.027552, -0.264043],
    [-0.295728, -0.256696],
    [-0.222255, -0.256696],
    [-0.156130, -0.256696],
    [0.130414, -0.256696],
    [-0.189192, -0.249348],
    [0.023879, -0.249348],
    [0.170824, -0.249348],
    [-0.236950, -0.242001],
    [0.071636, -0.242001],
    [0.086331, -0.242001],
    [0.115720, -0.242001],
    [0.038573, -0.234654],
    [0.163477, -0.234654],
    [-0.214908, -0.227306],
    [0.134088, -0.219959],
    [0.071636, -0.212612],
    [-0.332464, -0.197917],
    [-0.303075, -0.197917],
    [0.056941, -0.197917],
    [0.086331, -0.197917],
    [-0.347159, -0.183223],
    [-0.383895, -0.168528],
    [-0.354506, -0.168528],
    [-0.369201, -0.153834],
    [-0.339812, -0.153834],
    [-0.354506, -0.139139],
    [-0.409611, -0.131792],
    [-0.339812, -0.124445],
    [-0.394916, -0.117097],
    [-0.409611, -0.102403],
    [-0.424305, -0.087708],
    [-0.416958, -0.073014],
    [-0.402264, -0.073014],
    [-0.328791, -0.073014],
    [-0.310423, -0.065666],
    [0.277360, -0.065666],
    [0.314096, -0.065666],
    [0.343485, -0.065666],
    [-0.394916, -0.058319],
    [-0.343485, -0.058319],
    [0.292054, -0.050972],
    [-0.328791, -0.043624],
    [0.354506, -0.043624],
    [0.405937, -0.043624],
    [-0.435326, -0.036277],
    [-0.361854, -0.036277],
    [0.277360, -0.036277],
    [0.347159, -0.028930],
    [-0.420632, -0.021583],
    [-0.369201, -0.021583],
    [0.086331, -0.021583],
    [0.292054, -0.021583],
    [-0.402264, -0.014235],
    [-0.435326, -0.006888],
    [0.056941, -0.006888],
    [0.203887, -0.006888],
    [0.299402, -0.006888],
    [-0.350833, 0.000459],
    [0.236950, 0.000459],
    [0.078983, 0.007806],
    [0.270013, 0.007806],
    [0.380222, 0.007806],
    [0.023879, 0.015154],
    [0.222255, 0.015154],
    [0.361854, 0.015154],
    [-0.435326, 0.022501],
    [-0.001837, 0.022501],
    [0.086331, 0.022501],
    [0.101025, 0.022501],
    [0.159803, 0.022501],
    [0.255318, 0.022501],
    [0.314096, 0.022501],
    [0.328791, 0.022501],
    [-0.328791, 0.029848],
    [-0.299402, 0.029848],
    [0.134088, 0.029848],
    [0.222255, 0.029848],
    [-0.398590, 0.037196],
    [0.064289, 0.037196],
    [0.078983, 0.037196],
    [0.167151, 0.037196],
    [0.277360, 0.037196],
    [-0.336138, 0.044543],
    [-0.306749, 0.044543],
    [-0.056941, 0.044543],
    [0.200213, 0.044543],
    [-0.082657, 0.051890],
    [-0.001837, 0.051890],
    [0.034900, 0.051890],
    [0.093678, 0.051890],
    [0.174498, 0.051890],
    [-0.299402, 0.059237],
    [-0.049594, 0.059237],
    [0.214908, 0.059237],
    [0.405937, 0.059237],
    [0.012858, 0.066585],
    [0.247971, 0.066585],
    [-0.321444, 0.073932],
    [0.126741, 0.073932],
    [0.222255, 0.073932],
    [-0.427979, 0.081279],
    [-0.273686, 0.081279],
    [-0.200213, 0.081279],
    [0.005510, 0.081279],
    [0.042247, 0.081279],
    [0.167151, 0.081279],
    [-0.394916, 0.088627],
    [-0.380222, 0.088627],
    [-0.306749, 0.088627],
    [-0.137762, 0.088627],
    [0.075310, 0.088627],
    [0.141435, 0.088627],
    [-0.192866, 0.095974],
    [-0.023879, 0.095974],
    [0.108372, 0.095974],
    [-0.409611, 0.103321],
    [-0.380222, 0.103321],
    [0.126741, 0.103321],
    [0.207561, 0.103321],
    [-0.207561, 0.110668],
    [-0.178172, 0.110668],
    [-0.067962, 0.110668],
    [-0.038573, 0.110668],
    [-0.009184, 0.110668],
    [-0.343485, 0.118016],
    [-0.292054, 0.118016],
    [-0.123067, 0.118016],
    [-0.156130, 0.125363],
    [-0.075310, 0.125363],
    [-0.023879, 0.125363],
    [0.064289, 0.125363],
    [0.093678, 0.125363],
    [0.145109, 0.125363],
    [-0.284707, 0.132710],
    [-0.115720, 0.132710],
    [-0.391243, 0.140058],
    [-0.001837, 0.140058],
    [0.145109, 0.140058],
    [0.174498, 0.140058],
    [-0.123067, 0.147405],
    [-0.266339, 0.154752],
    [0.012858, 0.154752],
    [0.130414, 0.154752],
    [-0.145109, 0.162099],
    [0.045921, 0.162099],
    [0.369201, 0.162099],
    [-0.273686, 0.169447],
    [-0.200213, 0.169447],
    [-0.090004, 0.169447],
    [0.078983, 0.169447],
    [0.108372, 0.169447],
    [-0.372874, 0.176794],
    [-0.343485, 0.176794],
    [0.023879, 0.176794],
    [0.141435, 0.176794],
    [-0.266339, 0.184141],
    [-0.358180, 0.191488],
    [-0.343485, 0.191488],
    [0.038573, 0.191488],
    [0.053268, 0.191488],
    [0.126741, 0.191488],
    [-0.288381, 0.198836],
    [-0.214908, 0.198836],
    [-0.038573, 0.198836],
    [0.343485, 0.198836],
    [-0.336138, 0.206183],
    [-0.255318, 0.206183],
    [-0.196540, 0.206183],
    [-0.170824, 0.213530],
    [-0.053268, 0.213530],
    [-0.001837, 0.213530],
    [-0.277360, 0.220878],
    [-0.262665, 0.220878],
    [0.053268, 0.220878],
    [-0.148782, 0.228225],
    [0.012858, 0.228225],
    [0.321444, 0.228225],
    [-0.181845, 0.235572],
    [-0.093678, 0.235572],
    [-0.034900, 0.235572],
    [0.067962, 0.235572],
    [0.097351, 0.235572],
    [-0.163477, 0.242919],
    [0.167151, 0.242919],
    [0.299402, 0.242919],
    [-0.328791, 0.250267],
    [-0.255318, 0.250267],
    [-0.078983, 0.250267],
    [0.034900, 0.257614],
    [-0.262665, 0.264961],
    [0.281033, 0.264961],
    [-0.207561, 0.272309],
    [0.020205, 0.272309],
    [-0.236950, 0.287003],
    [-0.222255, 0.287003],
    [0.027552, 0.287003],
    [0.042247, 0.287003],
    [-0.086331, 0.294350],
    [-0.056941, 0.294350],
    [-0.042247, 0.294350],
    [-0.012858, 0.294350],
    [0.236950, 0.294350],
    [-0.251644, 0.301698],
];

};
factories["dot-pixel-body"]=function(exports,require){
"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.DOT_PIXEL_BODY = void 0;
exports.DOT_PIXEL_BODY = [
    [-0.070881, -0.368120, 0, 215, 232],
    [0.057161, 0.363547, 11, 111, 255],
    [-0.404703, 0.066307, 56, 65, 255],
    [0.395557, -0.052589, 0, 215, 232],
    [0.034297, -0.002287, 11, 111, 255],
    [-0.331536, -0.221787, 11, 111, 255],
    [0.308672, 0.221786, 3, 167, 255],
    [-0.221787, 0.285807, 56, 65, 255],
    [0.185204, -0.240078, 41, 247, 187],
    [-0.180631, 0.066307, 56, 65, 255],
    [0.116610, 0.176057, 11, 111, 255],
    [0.221786, 0.020579, 3, 167, 255],
    [-0.061735, 0.203494, 56, 65, 255],
    [0.006860, -0.208068, 0, 215, 232],
    [-0.294953, -0.057162, 56, 65, 255],
    [-0.166911, -0.230932, 3, 167, 255],
    [-0.093745, 0.363547, 76, 25, 214],
    [0.390985, 0.098318, 3, 167, 255],
    [-0.345256, 0.203494, 76, 25, 214],
    [0.194348, 0.313244, 11, 111, 255],
    [0.057161, -0.336109, 0, 215, 232],
    [-0.057161, 0.080025, 11, 111, 255],
    [-0.390984, -0.121182, 11, 111, 255],
    [-0.230932, 0.171485, 56, 65, 255],
    [-0.253796, -0.304100, 3, 167, 255],
    [-0.294953, 0.080025, 56, 65, 255],
    [0.038869, 0.253797, 11, 111, 255],
    [0.121182, 0.066307, 11, 111, 255],
    [0.290380, -0.061734, 0, 215, 232],
    [0.249224, 0.134900, 3, 167, 255],
    [-0.249224, -0.162339, 11, 111, 255],
    [0.134902, -0.034296, 3, 167, 255],
    [0.020578, 0.144047, 11, 111, 255],
    [0.102890, -0.185203, 0, 215, 232],
    [-0.075453, -0.267515, 3, 167, 255],
    [0.322391, 0.029724, 3, 167, 255],
    [-0.162339, -0.336109, 3, 167, 255],
    [-0.125756, 0.272088, 56, 65, 255],
    [-0.409276, -0.029725, 56, 65, 255],
    [-0.139475, 0.153193, 56, 65, 255],
    [-0.038870, 0.294954, 56, 65, 255],
    [0.221786, 0.230932, 11, 111, 255],
    [0.112037, 0.299525, 11, 111, 255],
    [0.102891, -0.267515, 0, 215, 232],
    [-0.336110, 0.011432, 56, 65, 255],
    [-0.166912, 0.340682, 56, 65, 255],
    [0.413850, 0.025150, 0, 215, 232],
    [-0.363546, 0.130329, 76, 25, 214],
    [-0.002287, -0.290381, 0, 215, 232],
    [-0.294953, 0.258369, 76, 25, 214],
    [0.176057, 0.116610, 3, 167, 255],
    [0.340682, 0.153193, 0, 215, 232],
    [0.208068, -0.052589, 0, 215, 232],
    [-0.317818, -0.134901, 56, 65, 255],
    [0.043443, 0.070880, 11, 111, 255],
    [-0.016006, 0.368119, 76, 25, 214],
    [-0.171485, 0.217213, 56, 65, 255],
    [-0.240078, -0.235506, 3, 167, 255],
    [-0.294953, 0.148619, 56, 65, 255],
    [0.148619, 0.235505, 3, 167, 255],
    [-0.002286, -0.363546, 0, 215, 232],
    [-0.235506, 0.107463, 11, 111, 255],
    [-0.025151, 0.020579, 11, 111, 255],
    [-0.121183, 0.089172, 11, 111, 255],
    [0.084599, 0.121182, 11, 111, 255],
    [-0.075453, 0.139474, 11, 111, 255],
    [0.057161, 0.194349, 11, 111, 255],
    [-0.358975, -0.066307, 56, 65, 255],
    [0.093745, 0.011432, 3, 167, 255],
    [-0.002286, 0.208068, 11, 111, 255],
    [-0.134900, -0.281234, 3, 167, 255],
    [-0.194348, -0.285807, 3, 167, 255],
    [0.340682, -0.029725, 0, 215, 232],
    [0.162338, 0.020579, 3, 167, 255],
    [0.226359, 0.080025, 3, 167, 255],
    [0.267516, 0.267516, 11, 111, 255],
    [0.043443, -0.253796, 0, 215, 232],
    [0.016005, 0.313244, 56, 65, 255],
    [-0.276662, 0.025150, 56, 65, 255],
    [-0.240078, 0.230932, 56, 65, 255],
    [0.272088, -0.006859, 0, 215, 232],
    [-0.363546, -0.171484, 56, 65, 255],
    [0.262943, 0.189775, 3, 167, 255],
    [-0.116609, 0.212641, 56, 65, 255],
    [-0.294953, -0.267515, 11, 111, 255],
    [-0.349828, 0.070880, 56, 65, 255],
    [-0.290381, 0.203494, 76, 25, 214],
    [-0.093745, -0.317818, 3, 167, 255],
    [0.272089, 0.048016, 3, 167, 255],
    [0.322391, 0.102891, 3, 167, 255],
    [-0.189776, 0.134900, 11, 111, 255],
    [-0.006860, 0.098318, 11, 111, 255],
    [0.134901, -0.226359, 0, 215, 232],
    [-0.281234, -0.203495, 3, 167, 255],
    [-0.089172, 0.308672, 56, 65, 255],
    [-0.038870, -0.326964, 0, 215, 232],
    [0.148620, 0.336110, 11, 111, 255],
    [0.212641, 0.180630, 3, 167, 255],
    [-0.029724, 0.162338, 11, 111, 255],
    [-0.029725, -0.244651, 0, 215, 232],
    [-0.386411, 0.016005, 56, 65, 255],
    [0.052588, -0.185203, 3, 167, 255],
    [0.084600, 0.235505, 11, 111, 255],
    [-0.080026, 0.249224, 56, 65, 255],
    [-0.176057, 0.267516, 56, 65, 255],
    [-0.240078, 0.057161, 56, 65, 255],
    [-0.093745, 0.048016, 11, 111, 255],
    [0.180630, 0.066307, 3, 167, 255],
    [0.368120, 0.011432, 0, 215, 232],
    [0.102890, 0.349829, 56, 65, 255],
    [-0.121182, -0.358975, 3, 167, 255],
    [-0.025151, 0.249224, 56, 65, 255],
    [0.130329, 0.130329, 11, 111, 255],
    [0.157766, 0.281235, 11, 111, 255],
    [-0.208068, -0.331537, 3, 167, 255],
    [0.294954, 0.148619, 3, 167, 255],
    [-0.281234, -0.107464, 56, 65, 255],
    [-0.052589, 0.340682, 56, 65, 255],
    [0.080025, -0.226359, 0, 215, 232],
    [0.221786, 0.276661, 11, 111, 255],
    [-0.208068, -0.203495, 3, 167, 255],
    [0.176057, -0.020578, 0, 215, 232],
    [-0.317817, -0.180631, 11, 111, 255],
    [-0.299526, -0.011432, 11, 111, 255],
    [-0.326964, 0.107463, 56, 65, 255],
    [0.016005, 0.038869, 3, 167, 255],
    [0.180630, 0.208068, 11, 111, 255],
    [-0.400131, -0.080026, 56, 65, 255],
    [-0.130328, 0.317818, 56, 65, 255],
    [-0.326964, -0.093745, 11, 111, 255],
    [0.249224, -0.043443, 3, 167, 255],
    [-0.180631, 0.176057, 56, 65, 255],
    [-0.336109, 0.162338, 56, 65, 255],
    [-0.363546, -0.020578, 11, 111, 255],
    [0.386411, 0.057161, 3, 167, 255],
    [-0.162338, 0.102891, 56, 65, 255],
    [-0.102890, 0.171485, 56, 65, 255],
    [0.080027, 0.052588, 3, 167, 255],
    [0.016005, -0.326964, 0, 215, 232],
    [0.413848, -0.016006, 0, 215, 232],
    [-0.313244, 0.043443, 56, 65, 255],
    [0.043443, 0.112036, 11, 111, 255],
    [0.189775, 0.253797, 11, 111, 255],
    [-0.038870, 0.121182, 11, 111, 255],
    [-0.112036, -0.249225, 3, 167, 255],
    [0.080025, 0.162338, 11, 111, 255],
    [-0.258370, 0.272088, 76, 25, 214],
    [-0.020578, 0.061735, 11, 111, 255],
    [-0.390984, 0.102891, 76, 25, 214],
    [0.336110, 0.194349, 3, 167, 255],
    [-0.208068, 0.203494, 76, 25, 214],
    [0.002286, 0.276661, 56, 65, 255],
    [0.020578, 0.354400, 56, 65, 255],
    [-0.203495, -0.249225, 3, 167, 255],
    [-0.112037, 0.125755, 56, 65, 255],
    [-0.258370, 0.139474, 56, 65, 255],
    [-0.038870, -0.281234, 0, 215, 232],
    [-0.285807, -0.153193, 11, 111, 255],
    [-0.203495, 0.240079, 56, 65, 255],
    [0.217213, -0.016006, 0, 215, 232],
    [0.372692, 0.130329, 3, 167, 255],
    [-0.326964, -0.038870, 11, 111, 255],
    [-0.198921, 0.098318, 11, 111, 255],
    [0.308672, -0.011432, 0, 215, 232],
    [0.125755, 0.029724, 3, 167, 255],
    [-0.354401, -0.125756, 56, 65, 255],
    [0.006860, -0.249225, 3, 167, 255],
    [-0.235506, -0.272089, 11, 111, 255],
    [0.358974, -0.061734, 0, 215, 232],
    [-0.057161, 0.038869, 11, 111, 255],
    [-0.244650, -0.198921, 11, 111, 255],
    [-0.130329, -0.317818, 3, 167, 255],
    [-0.016005, 0.331536, 56, 65, 255],
    [-0.130328, 0.354400, 76, 25, 214],
    [-0.144046, 0.240079, 76, 25, 214],
    [0.148620, 0.089172, 3, 167, 255],
    [-0.144047, 0.189775, 56, 65, 255],
    [-0.267515, 0.176057, 56, 65, 255],
    [-0.317818, 0.226360, 56, 65, 255],
    [0.052588, 0.029724, 3, 167, 255],
    [0.070880, -0.016006, 3, 167, 255],
    [0.020579, 0.180630, 11, 111, 255],
    [-0.084600, 0.102891, 11, 111, 255],
    [-0.144047, 0.061735, 11, 111, 255],
    [-0.272089, 0.107463, 56, 65, 255],
    [0.116610, 0.249224, 11, 111, 255],
    [-0.368120, 0.171485, 76, 25, 214],
    [-0.358975, 0.038869, 56, 65, 255],
    [0.089172, 0.084599, 3, 167, 255],
    [0.043443, -0.217214, 0, 215, 232],
    [0.029724, 0.221786, 56, 65, 255],
    [0.276661, 0.230932, 11, 111, 255],
    [0.070881, -0.276662, 0, 215, 232],
    [0.093744, 0.203494, 11, 111, 255],
    [0.102891, -0.025151, 3, 167, 255],
    [0.377266, -0.020578, 0, 215, 232],
    [-0.162339, -0.299526, 3, 167, 255],
    [-0.299526, -0.230932, 3, 167, 255],
    [-0.066307, -0.299526, 0, 215, 232],
    [-0.070880, 0.281235, 56, 65, 255],
    [0.290380, 0.020579, 0, 215, 232],
    [-0.034297, -0.368120, 0, 215, 232],
    [-0.166912, -0.267515, 3, 167, 255],
    [0.322391, -0.057162, 0, 215, 232],
    [-0.221786, 0.139474, 56, 65, 255],
    [0.006860, 0.244650, 56, 65, 255],
    [0.038869, 0.290380, 11, 111, 255],
    [-0.272089, 0.235505, 56, 65, 255],
    [0.130328, -0.002287, 3, 167, 255],
    [-0.102891, -0.285807, 3, 167, 255],
    [-0.066307, 0.171485, 56, 65, 255],
    [0.139473, -0.194350, 0, 215, 232],
    [0.253797, 0.020579, 0, 215, 232],
    [0.011432, 0.070880, 3, 167, 255],
    [-0.272088, 0.057161, 56, 65, 255],
    [0.048016, 0.162338, 11, 111, 255],
    [-0.034296, 0.217213, 11, 111, 255],
    [0.235506, 0.048016, 3, 167, 255],
    [-0.272088, -0.244651, 11, 111, 255],
    [-0.011432, 0.134900, 11, 111, 255],
    [0.198921, 0.093744, 3, 167, 255],
    [-0.368120, -0.098318, 56, 65, 255],
    [-0.112036, 0.244650, 56, 65, 255],
    [0.194349, 0.038869, 3, 167, 255],
    [0.189776, 0.006860, 3, 167, 255],
    [-0.221787, -0.299526, 3, 167, 255],
    [0.336110, 0.002286, 0, 215, 232],
    [0.148619, 0.052588, 3, 167, 255],
    [-0.212640, 0.070880, 56, 65, 255],
    [0.116610, 0.098318, 11, 111, 255],
    [0.144047, 0.157766, 3, 167, 255],
    [-0.253796, 0.203494, 76, 25, 214],
    [-0.194350, 0.326963, 76, 25, 214],
    [0.070880, 0.336110, 56, 65, 255],
    [-0.308671, 0.176057, 76, 25, 214],
    [0.029723, -0.354401, 0, 215, 232],
    [-0.386411, -0.052589, 56, 65, 255],
    [-0.349828, -0.198921, 11, 111, 255],
    [0.025152, -0.276662, 0, 215, 232],
    [0.153192, -0.249225, 0, 215, 232],
    [-0.409276, 0.034297, 56, 65, 255],
    [0.102891, 0.144047, 3, 167, 255],
    [0.162339, -0.212640, 0, 215, 232],
    [-0.144047, -0.249225, 3, 167, 255],
    [-0.107463, 0.336110, 56, 65, 255],
    [-0.070881, -0.336109, 3, 167, 255],
    [-0.253797, 0.084599, 56, 65, 255],
    [-0.336109, -0.157765, 11, 111, 255],
    [0.230932, 0.157766, 11, 111, 255],
    [0.244650, 0.212641, 3, 167, 255],
    [-0.153193, 0.285807, 76, 25, 214],
    [-0.267515, -0.276662, 11, 111, 255],
    [0.107463, -0.235506, 0, 215, 232],
    [0.006860, 0.006860, 11, 111, 255],
    [-0.358975, 0.098318, 76, 25, 214],
    [0.281235, -0.034296, 0, 215, 232],
    [0.121182, 0.326963, 11, 111, 255],
    [-0.230932, 0.258369, 56, 65, 255],
    [-0.089171, 0.221786, 56, 65, 255],
    [-0.011432, -0.317818, 0, 215, 232],
    [-0.061734, 0.368119, 76, 25, 214],
    [-0.299525, 0.116610, 56, 65, 255],
    [-0.052588, 0.258369, 56, 65, 255],
    [-0.194348, 0.290380, 56, 65, 255],
    [0.057161, 0.230932, 56, 65, 255],
    [-0.322390, 0.134900, 76, 25, 214],
    [-0.299526, -0.084600, 11, 111, 255],
    [-0.322390, -0.066307, 56, 65, 255],
    [0.244650, -0.011432, 3, 167, 255],
    [-0.139475, 0.121182, 11, 111, 255],
    [-0.304100, 0.016005, 56, 65, 255],
    [-0.025151, 0.189775, 11, 111, 255],
    [0.162339, 0.308672, 11, 111, 255],
    [0.016005, 0.116610, 11, 111, 255],
    [-0.011432, 0.304099, 56, 65, 255],
    [0.066307, 0.258369, 56, 65, 255],
    [-0.061734, 0.313244, 56, 65, 255],
    [-0.377265, 0.061735, 56, 65, 255],
    [-0.089171, 0.075454, 56, 65, 255],
    [-0.322390, 0.075454, 56, 65, 255],
    [-0.409276, -0.002287, 56, 65, 255],
    [-0.002286, 0.162338, 11, 111, 255],
    [0.308672, 0.194349, 3, 167, 255],
    [-0.080025, 0.336110, 56, 65, 255],
    [0.075454, -0.198921, 3, 167, 255],
    [-0.116609, 0.061735, 11, 111, 255],
    [-0.034296, 0.093744, 56, 65, 255],
    [-0.203495, 0.162338, 56, 65, 255],
    [-0.057161, 0.230932, 56, 65, 255],
    [-0.098318, -0.345256, 3, 167, 255],
    [-0.166911, 0.148619, 56, 65, 255],
    [0.043443, 0.340682, 11, 111, 255],
    [-0.185204, -0.317818, 3, 167, 255],
    [0.038869, -0.313245, 0, 215, 232],
    [0.066307, 0.098318, 3, 167, 255],
    [-0.377265, -0.144046, 11, 111, 255],
    [-0.016006, -0.267515, 0, 215, 232],
    [-0.272088, -0.176057, 11, 111, 255],
    [-0.089172, 0.194349, 56, 65, 255],
    [0.125756, -0.253796, 0, 215, 232],
    [0.395557, 0.002286, 0, 215, 232],
    [0.208067, 0.061735, 3, 167, 255],
    [0.281235, 0.171485, 3, 167, 255],
    [-0.267516, -0.134901, 11, 111, 255],
    [0.116610, -0.208068, 0, 215, 232],
    [-0.020578, -0.345256, 0, 215, 232],
    [0.157766, 0.134900, 3, 167, 255],
    [-0.057162, -0.249225, 3, 167, 255],
    [-0.317818, 0.198922, 56, 65, 255],
    [0.244650, 0.258369, 11, 111, 255],
    [-0.048016, 0.144047, 11, 111, 255],
    [-0.194350, -0.226359, 3, 167, 255],
    [0.070880, -0.249225, 0, 215, 232],
    [0.166911, -0.043443, 3, 167, 255],
    [0.025150, 0.093744, 11, 111, 255],
    [0.029724, -0.194350, 0, 215, 232],
    [-0.157765, 0.317818, 76, 25, 214],
    [0.061735, 0.006860, 11, 111, 255],
    [0.313244, 0.125755, 0, 215, 232],
    [-0.112036, 0.299525, 56, 65, 255],
    [-0.363547, 0.006860, 56, 65, 255],
    [-0.386411, -0.011432, 56, 65, 255],
    [0.102891, 0.043443, 3, 167, 255],
    [-0.203495, 0.267516, 56, 65, 255],
    [-0.308672, -0.208068, 11, 111, 255],
    [-0.308672, -0.112037, 11, 111, 255],
    [-0.326964, -0.011432, 56, 65, 255],
    [0.134901, 0.290380, 11, 111, 255],
    [-0.061734, 0.112036, 56, 65, 255],
    [-0.262943, -0.221787, 3, 167, 255],
    [0.153193, -0.011432, 3, 167, 255],
    [0.409276, 0.048016, 0, 215, 232],
    [0.368119, 0.102891, 3, 167, 255],
    [-0.171484, 0.244650, 56, 65, 255],
    [-0.098318, 0.148619, 56, 65, 255],
    [-0.148619, 0.212641, 56, 65, 255],
    [-0.102890, 0.267516, 56, 65, 255],
    [-0.308672, -0.157765, 11, 111, 255],
    [0.390985, 0.029724, 0, 215, 232],
    [0.089172, 0.304099, 11, 111, 255],
    [-0.144047, -0.354401, 3, 167, 255],
    [-0.290381, -0.130328, 11, 111, 255],
    [-0.336109, 0.048016, 56, 65, 255],
    [-0.294953, 0.230932, 76, 25, 214],
    [0.171485, 0.093744, 3, 167, 255],
    [-0.354400, -0.043443, 56, 65, 255],
    [0.043442, 0.139474, 11, 111, 255],
    [-0.148620, 0.262943, 56, 65, 255],
    [-0.029725, 0.272088, 56, 65, 255],
    [0.171485, 0.043443, 3, 167, 255],
    [0.198922, 0.290380, 11, 111, 255],
    [0.080025, 0.358974, 11, 111, 255],
    [-0.043443, -0.304100, 0, 215, 232],
    [-0.230932, 0.208068, 56, 65, 255],
    [0.153193, 0.112036, 3, 167, 255],
    [0.313244, -0.034296, 3, 167, 255],
    [-0.304098, -0.034296, 56, 65, 255],
    [-0.386412, 0.125755, 76, 25, 214],
    [-0.038869, 0.363547, 56, 65, 255],
    [-0.294954, -0.185203, 11, 111, 255],
    [-0.212641, 0.116610, 56, 65, 255],
    [-0.070881, 0.057161, 56, 65, 255],
    [0.075454, 0.029724, 11, 111, 255],
    [0.363547, 0.153193, 3, 167, 255],
    [0.244650, 0.281235, 11, 111, 255],
    [-0.121183, 0.185204, 56, 65, 255],
    [-0.034297, 0.043443, 11, 111, 255],
    [0.363546, -0.038870, 0, 215, 232],
    [-0.166911, 0.194349, 56, 65, 255],
    [-0.052589, -0.349828, 0, 215, 232],
    [-0.313245, -0.249225, 11, 111, 255],
    [-0.011432, -0.230932, 3, 167, 255],
    [0.020579, -0.230932, 0, 215, 232],
    [-0.006859, 0.034297, 3, 167, 255],
    [-0.162339, 0.080025, 11, 111, 255],
    [-0.034297, 0.317818, 56, 65, 255],
    [-0.331537, -0.116609, 11, 111, 255],
    [0.230932, -0.057162, 0, 215, 232],
    [0.267516, -0.057162, 0, 215, 232],
    [0.189775, -0.038870, 3, 167, 255],
    [0.038869, 0.048016, 11, 111, 255],
    [0.400130, 0.075454, 0, 215, 232],
    [0.377266, 0.080025, 0, 215, 232],
    [0.326963, 0.171485, 3, 167, 255],
    [-0.253796, 0.249224, 76, 25, 214],
    [-0.011432, 0.230932, 11, 111, 255],
    [-0.217214, -0.230932, 3, 167, 255],
    [-0.230932, 0.084599, 56, 65, 255],
    [-0.212640, -0.272089, 3, 167, 255],
    [0.048016, -0.276662, 0, 215, 232],
    [0.253797, 0.061735, 3, 167, 255],
    [-0.048015, 0.185204, 11, 111, 255],
    [0.349828, 0.176057, 3, 167, 255],
    [-0.290380, 0.043443, 56, 65, 255],
    [0.034296, 0.198922, 11, 111, 255],
    [0.354401, -0.011432, 0, 215, 232],
    [-0.157766, 0.171485, 56, 65, 255],
    [-0.002287, 0.185204, 11, 111, 255],
    [-0.089172, -0.249225, 3, 167, 255],
    [-0.185203, -0.340682, 3, 167, 255],
    [-0.386412, 0.038869, 56, 65, 255],
    [0.066307, 0.075454, 11, 111, 255],
    [-0.166912, 0.125755, 56, 65, 255],
    [0.016005, -0.304100, 0, 215, 232],
    [0.107463, 0.121182, 3, 167, 255],
    [0.093744, 0.180630, 3, 167, 255],
    [0.221786, 0.299525, 11, 111, 255],
    [0.029724, 0.020579, 11, 111, 255],
    [-0.281234, 0.130329, 76, 25, 214],
    [-0.377265, 0.148619, 76, 25, 214],
    [0.070880, 0.212641, 11, 111, 255],
    [0.025150, 0.272088, 56, 65, 255],
    [-0.144046, 0.336110, 56, 65, 255],
    [-0.002287, 0.349829, 56, 65, 255],
    [-0.253796, -0.258370, 3, 167, 255],
    [-0.377265, 0.084599, 56, 65, 255],
    [-0.180630, -0.249225, 3, 167, 255],
    [-0.349828, -0.089171, 11, 111, 255],
    [-0.249225, 0.162338, 56, 65, 255],
    [-0.226359, -0.253796, 11, 111, 255],
    [-0.390984, -0.098318, 56, 65, 255],
    [0.230932, -0.034296, 3, 167, 255],
    [-0.144046, 0.089172, 11, 111, 255],
    [-0.345256, 0.180630, 56, 65, 255],
    [-0.272089, 0.212641, 76, 25, 214],
    [0.308672, 0.011432, 0, 215, 232],
    [0.294954, 0.240079, 11, 111, 255],
    [-0.340683, 0.125755, 56, 65, 255],
    [-0.121182, -0.299526, 3, 167, 255],
    [-0.340682, 0.089172, 56, 65, 255],
    [0.349829, 0.020579, 3, 167, 255],
    [0.176058, 0.272088, 11, 111, 255],
    [0.093744, 0.331536, 56, 65, 255],
    [-0.043443, 0.061735, 11, 111, 255],
    [0.061735, 0.121182, 11, 111, 255],
    [-0.308672, 0.098318, 56, 65, 255],
    [-0.354401, 0.153193, 56, 65, 255],
    [-0.121182, 0.162338, 56, 65, 255],
    [-0.230932, -0.317818, 3, 167, 255],
    [0.098318, 0.066307, 3, 167, 255],
    [0.157766, 0.070880, 3, 167, 255],
    [0.198922, 0.198922, 11, 111, 255],
    [0.212641, 0.249224, 11, 111, 255],
    [0.098318, -0.217214, 0, 215, 232],
    [0.262943, -0.025151, 0, 215, 232],
    [-0.285807, 0.166911, 56, 65, 255],
    [-0.189776, 0.194349, 56, 65, 255],
    [-0.194350, 0.221786, 56, 65, 255],
    [0.395557, -0.029725, 41, 247, 187],
    [-0.377265, -0.080026, 11, 111, 255],
    [-0.116609, -0.336109, 3, 167, 255],
    [-0.226359, -0.189776, 11, 111, 255],
    [-0.203494, -0.308671, 3, 167, 255],
    [-0.253796, 0.116610, 56, 65, 255],
    [-0.326964, -0.198921, 56, 65, 255],
    [0.006860, -0.345256, 0, 215, 232],
    [-0.212641, 0.180630, 56, 65, 255],
    [0.139473, 0.317818, 11, 111, 255],
    [-0.354400, -0.148620, 11, 111, 255],
    [0.048015, 0.272088, 11, 111, 255],
    [-0.340682, -0.180631, 11, 111, 255],
    [-0.153193, -0.317818, 3, 167, 255],
    [0.121182, 0.153193, 3, 167, 255],
    [0.171484, 0.326963, 11, 111, 255],
    [-0.285807, 0.006860, 56, 65, 255],
    [-0.304098, 0.061735, 56, 65, 255],
    [-0.093745, 0.285807, 56, 65, 255],
    [0.052588, -0.235506, 0, 215, 232],
    [-0.226359, -0.212640, 3, 167, 255],
    [-0.180631, 0.089172, 11, 111, 255],
    [-0.020578, -0.299526, 0, 215, 232],
    [0.070881, 0.180630, 11, 111, 255],
    [0.048016, 0.212641, 56, 65, 255],
    [-0.006860, 0.258369, 56, 65, 255],
    [-0.107464, 0.102891, 56, 65, 255],
    [-0.029724, 0.345255, 56, 65, 255],
    [-0.377266, -0.034296, 56, 65, 255],
    [-0.322391, 0.025150, 56, 65, 255],
    [-0.244651, 0.185204, 76, 25, 214],
    [-0.125756, -0.262943, 3, 167, 255],
    [-0.125756, 0.230932, 56, 65, 255],
    [0.235505, 0.006860, 3, 167, 255],
    [0.166911, 0.221786, 11, 111, 255],
    [-0.217214, 0.221786, 76, 25, 214],
    [-0.057162, -0.317818, 0, 215, 232],
    [0.093744, -0.249225, 0, 215, 232],
    [0.116609, 0.011432, 3, 167, 255],
    [0.144046, 0.011432, 3, 167, 255],
    [0.002286, 0.052588, 3, 167, 255],
    [0.057161, 0.057161, 11, 111, 255],
    [-0.372693, 0.112036, 76, 25, 214],
    [-0.185203, 0.112036, 56, 65, 255],
    [-0.020579, 0.112036, 11, 111, 255],
    [-0.089171, 0.125755, 11, 111, 255],
    [-0.272089, 0.153193, 56, 65, 255],
    [0.258369, 0.244650, 3, 167, 255],
    [0.016005, 0.290380, 56, 65, 255],
    [0.180630, 0.299525, 11, 111, 255],
    [0.006859, 0.368119, 56, 65, 255],
    [-0.349828, -0.006859, 56, 65, 255],
    [0.281234, 0.253797, 3, 167, 255],
    [-0.125755, 0.139474, 56, 65, 255],
    [-0.084600, 0.162338, 56, 65, 255],
    [-0.189775, 0.253797, 56, 65, 255],
    [-0.340682, -0.057162, 56, 65, 255],
    [-0.340682, -0.025151, 56, 65, 255],
    [-0.308671, 0.244650, 56, 65, 255],
    [-0.272088, 0.258369, 76, 25, 214],
    [0.322390, 0.208068, 3, 167, 255],
    [-0.240079, 0.130329, 56, 65, 255],
    [-0.313245, 0.153193, 56, 65, 255],
    [0.217213, 0.043443, 3, 167, 255],
    [-0.262943, -0.194350, 3, 167, 255],
    [0.166911, -0.235506, 0, 215, 232],
    [0.166912, 0.002286, 3, 167, 255],
    [0.112036, 0.194349, 11, 111, 255],
    [0.034297, -0.336109, 0, 215, 232],
    [-0.089172, -0.299526, 3, 167, 255],
    [-0.061734, -0.281234, 3, 167, 255],
    [-0.240078, 0.276661, 56, 65, 255],
    [0.002286, -0.272089, 3, 167, 255],
    [0.061735, -0.212640, 3, 167, 255],
    [0.089172, -0.011432, 3, 167, 255],
    [0.208068, 0.002286, 3, 167, 255],
    [0.304099, 0.034297, 0, 215, 232],
    [0.048016, 0.089172, 3, 167, 255],
    [0.098318, 0.102891, 3, 167, 255],
    [0.290380, 0.217213, 3, 167, 255],
    [-0.098318, -0.267515, 0, 215, 232],
    [-0.016005, 0.285807, 56, 65, 255],
    [0.125755, 0.345255, 56, 65, 255],
    [0.194349, -0.011432, 0, 215, 232],
    [-0.139475, -0.336109, 3, 167, 255],
    [0.043443, 0.180630, 11, 111, 255],
    [-0.176057, 0.322391, 56, 65, 255],
    [-0.240078, -0.290381, 3, 167, 255],
    [-0.189777, -0.267515, 3, 167, 255],
    [-0.404703, -0.061734, 56, 65, 255],
    [0.180630, 0.025150, 3, 167, 255],
    [0.290380, -0.002287, 3, 167, 255],
    [-0.011432, 0.080025, 11, 111, 255],
    [-0.400131, 0.084599, 76, 25, 214],
    [0.121183, -0.020578, 3, 167, 255],
    [0.107464, -0.006859, 3, 167, 255],
    [0.130329, 0.084599, 11, 111, 255],
    [-0.029725, 0.139474, 11, 111, 255],
    [-0.048016, 0.276661, 56, 65, 255],
    [-0.340682, 0.029724, 56, 65, 255],
    [-0.272089, 0.080025, 56, 65, 255],
    [0.121182, -0.189776, 3, 167, 255],
    [0.038870, 0.358974, 56, 65, 255],
    [0.354400, 0.134900, 3, 167, 255],
    [0.377266, -0.057162, 41, 247, 187],
    [0.340682, -0.052589, 0, 215, 232],
    [0.326963, -0.016006, 0, 215, 232],
    [0.134902, 0.107463, 3, 167, 255],
    [-0.038870, 0.235505, 56, 65, 255],
    [-0.281235, -0.226359, 11, 111, 255],
    [-0.185203, 0.153193, 56, 65, 255],
    [0.084599, -0.180631, 3, 167, 255],
    [0.130328, 0.048016, 3, 167, 255],
    [0.016005, 0.203494, 11, 111, 255],
    [-0.267516, -0.157765, 11, 111, 255],
    [0.025151, 0.162338, 11, 111, 255],
    [0.272088, 0.016005, 3, 167, 255],
    [-0.089171, -0.363546, 3, 167, 255],
    [-0.043443, -0.262943, 3, 167, 255],
    [-0.112037, 0.358974, 56, 65, 255],
    [-0.125756, 0.107463, 11, 111, 255],
    [-0.372693, -0.116609, 56, 65, 255],
    [-0.075453, 0.354400, 56, 65, 255],
    [0.144047, 0.034297, 3, 167, 255],
    [0.011432, 0.226360, 11, 111, 255],
    [0.025150, 0.240079, 11, 111, 255],
    [0.025150, -0.212640, 3, 167, 255],
    [-0.130328, 0.253797, 56, 65, 255],
    [-0.290381, -0.249225, 3, 167, 255],
    [-0.217214, 0.098318, 56, 65, 255],
    [-0.244651, -0.217214, 11, 111, 255],
    [-0.249225, -0.180631, 11, 111, 255],
    [0.208067, -0.034296, 3, 167, 255],
    [-0.107464, 0.317818, 56, 65, 255],
    [-0.221787, 0.240079, 56, 65, 255],
    [0.025152, -0.258370, 0, 215, 232],
    [-0.349828, -0.107464, 11, 111, 255],
    [0.153193, -0.029725, 3, 167, 255],
    [-0.139474, 0.171485, 56, 65, 255],
    [-0.326964, 0.180630, 56, 65, 255],
    [-0.409276, 0.016005, 56, 65, 255],
    [0.253797, 0.043443, 3, 167, 255],
    [0.098318, 0.162338, 11, 111, 255],
    [-0.048015, 0.166911, 11, 111, 255],
    [-0.285807, 0.185204, 56, 65, 255],
    [-0.011432, -0.249225, 3, 167, 255],
    [-0.180630, -0.299526, 3, 167, 255],
    [0.203494, 0.020579, 3, 167, 255],
    [-0.052589, -0.368120, 0, 215, 232],
    [-0.112037, -0.317818, 3, 167, 255],
    [-0.144047, -0.299526, 3, 167, 255],
    [-0.153192, -0.281234, 3, 167, 255],
    [-0.162339, -0.249225, 3, 167, 255],
    [-0.075453, 0.038869, 11, 111, 255],
    [-0.290380, 0.098318, 56, 65, 255],
    [-0.002286, 0.116610, 11, 111, 255],
    [-0.057162, 0.294954, 56, 65, 255],
    [0.413849, 0.002286, 0, 215, 232],
    [-0.125755, 0.336110, 56, 65, 255],
    [-0.336109, -0.139475, 11, 111, 255],
    [-0.176057, 0.285807, 56, 65, 255],
    [-0.075453, -0.317818, 0, 215, 232],
    [-0.157766, 0.230932, 56, 65, 255],
    [-0.336110, 0.144047, 56, 65, 255],
    [-0.107463, 0.080025, 11, 111, 255],
    [-0.070881, 0.089172, 11, 111, 255],
    [-0.130328, 0.203494, 56, 65, 255],
    [-0.075453, 0.299525, 56, 65, 255],
    [-0.331536, 0.212641, 76, 25, 214],
    [-0.253797, 0.066307, 56, 65, 255],
    [0.006860, 0.089172, 11, 111, 255],
    [0.084600, -0.267515, 0, 215, 232],
    [-0.276662, -0.262943, 3, 167, 255],
    [-0.304100, 0.134900, 76, 25, 214],
    [-0.317817, 0.002286, 56, 65, 255],
    [0.093744, 0.029724, 11, 111, 255],
    [0.226359, 0.061735, 3, 167, 255],
    [0.194348, 0.075454, 3, 167, 255],
    [-0.194350, 0.080025, 56, 65, 255],
    [0.107463, 0.084599, 3, 167, 255],
    [-0.016005, 0.171485, 11, 111, 255],
    [-0.304100, 0.212641, 56, 65, 255],
    [-0.006859, -0.336109, 3, 167, 255],
    [0.080025, 0.002286, 3, 167, 255],
    [0.253797, 0.002286, 0, 215, 232],
    [0.052588, -0.006859, 3, 167, 255],
    [-0.034296, 0.075454, 11, 111, 255],
    [-0.043443, 0.203494, 56, 65, 255],
    [-0.294953, -0.166912, 11, 111, 255],
    [-0.267516, 0.038869, 56, 65, 255],
    [-0.358975, 0.057161, 56, 65, 255],
    [0.299527, -0.048015, 0, 215, 232],
    [0.020578, 0.057161, 11, 111, 255],
    [0.144047, -0.212640, 0, 215, 232],
    [0.285807, 0.038869, 3, 167, 255],
    [0.185203, 0.102891, 3, 167, 255],
    [-0.203494, 0.144047, 56, 65, 255],
    [0.002287, 0.322391, 56, 65, 255],
    [-0.235506, 0.148619, 76, 25, 214],
    [-0.107464, 0.194349, 56, 65, 255],
    [-0.093745, 0.240079, 56, 65, 255],
    [0.235505, 0.029724, 3, 167, 255],
    [0.093744, -0.198921, 3, 167, 255],
    [-0.029723, -0.313245, 0, 215, 232],
    [-0.080026, -0.285807, 0, 215, 232],
    [-0.148620, 0.139474, 56, 65, 255],
    [-0.221786, 0.157766, 56, 65, 255],
    [-0.212641, 0.253797, 56, 65, 255],
    [0.048016, -0.349828, 0, 215, 232],
    [0.377266, -0.002287, 0, 215, 232],
    [0.381838, 0.116610, 3, 167, 255],
    [-0.057162, 0.130329, 11, 111, 255],
    [-0.070881, 0.189775, 11, 111, 255],
    [-0.070881, 0.217213, 11, 111, 255],
    [0.034296, 0.125755, 11, 111, 255],
    [0.006860, 0.134900, 11, 111, 255],
    [0.153193, -0.226359, 0, 215, 232],
    [0.057161, -0.262943, 3, 167, 255],
    [0.290380, -0.020578, 0, 215, 232],
    [0.043443, 0.011432, 3, 167, 255],
    [-0.372693, 0.025150, 56, 65, 255],
    [0.006860, 0.025150, 11, 111, 255],
    [0.066307, 0.043443, 3, 167, 255],
    [-0.253796, 0.048016, 56, 65, 255],
    [-0.322390, 0.057161, 56, 65, 255],
    [0.139474, 0.066307, 11, 111, 255],
    [0.240079, 0.070880, 0, 215, 232],
    [-0.345256, 0.107463, 56, 65, 255],
    [-0.226359, 0.189775, 56, 65, 255],
    [-0.272089, 0.194349, 76, 25, 214],
    [-0.098318, 0.208068, 11, 111, 255],
    [-0.020578, 0.208068, 56, 65, 255],
    [-0.258370, 0.221786, 56, 65, 255],
    [-0.185203, 0.235505, 56, 65, 255],
    [0.016005, 0.258369, 11, 111, 255],
    [-0.080026, -0.349828, 0, 215, 232],
    [-0.171484, -0.285807, 3, 167, 255],
    [0.034297, -0.240078, 3, 167, 255],
    [-0.322391, -0.235506, 11, 111, 255],
    [-0.299527, -0.144046, 11, 111, 255],
    [-0.340682, -0.075453, 56, 65, 255],
    [-0.313245, -0.052589, 11, 111, 255],
    [-0.400130, -0.043443, 56, 65, 255],
    [-0.317818, -0.025151, 56, 65, 255],
    [-0.354402, 0.020579, 56, 65, 255],
    [-0.043442, 0.029724, 11, 111, 255],
    [-0.400131, 0.048016, 76, 25, 214],
    [-0.285808, 0.066307, 56, 65, 255],
    [-0.134901, 0.075454, 56, 65, 255],
    [-0.066307, 0.153193, 56, 65, 255],
    [-0.285807, 0.244650, 76, 25, 214],
    [-0.006859, 0.016005, 3, 167, 255],
    [0.080025, 0.194349, 11, 111, 255],
    [0.093744, 0.221786, 56, 65, 255],
    [-0.368119, -0.052589, 56, 65, 255],
    [-0.144047, -0.267515, 3, 167, 255],
    [-0.052589, 0.098318, 11, 111, 255],
    [-0.107464, 0.226360, 56, 65, 255],
    [0.130328, 0.166911, 11, 111, 255],
    [-0.016006, 0.153193, 11, 111, 255],
    [-0.093745, 0.345255, 56, 65, 255],
    [-0.093745, 0.326963, 56, 65, 255],
    [-0.116609, -0.276662, 3, 167, 255],
    [0.381839, -0.038870, 0, 215, 232],
    [-0.020578, -0.363546, 0, 215, 232],
    [-0.171484, -0.313245, 3, 167, 255],
    [0.025152, 0.075454, 11, 111, 255],
    [-0.208068, -0.290381, 11, 111, 255],
    [-0.070881, 0.235505, 11, 111, 255],
    [0.038870, 0.235505, 11, 111, 255],
    [-0.139473, 0.281235, 56, 65, 255],
    [-0.294953, -0.212640, 11, 111, 255],
    [0.409277, -0.034296, 0, 215, 232],
    [0.212641, 0.089172, 3, 167, 255],
    [0.139474, -0.016006, 3, 167, 255],
    [-0.372692, -0.006859, 56, 65, 255],
    [0.107464, 0.025150, 3, 167, 255],
    [-0.075453, 0.116610, 11, 111, 255],
    [0.070880, 0.240079, 11, 111, 255],
    [0.002286, 0.294954, 56, 65, 255],
    [-0.400131, -0.016006, 76, 25, 214],
    [-0.294953, -0.102890, 11, 111, 255],
    [-0.336109, 0.066307, 76, 25, 214],
    [0.002287, -0.226359, 3, 167, 255],
    [-0.066307, 0.262943, 56, 65, 255],
    [-0.093745, 0.089172, 11, 111, 255],
    [-0.368120, -0.157765, 11, 111, 255],
    [-0.089171, -0.331537, 0, 215, 232],
    [0.331536, 0.016005, 0, 215, 232],
    [0.386411, 0.016005, 0, 215, 232],
    [0.304098, 0.208068, 3, 167, 255],
    [-0.313245, 0.121182, 56, 65, 255],
    [-0.034297, -0.349828, 0, 215, 232],
    [0.208068, 0.308672, 11, 111, 255],
    [0.308672, -0.061734, 0, 215, 232],
    [-0.226359, 0.066307, 56, 65, 255],
    [0.016005, -0.358975, 0, 215, 232],
    [-0.066307, -0.354401, 0, 215, 232],
    [-0.217213, -0.313245, 3, 167, 255],
    [-0.107464, -0.304100, 0, 215, 232],
    [-0.272089, -0.290381, 11, 111, 255],
    [-0.253796, -0.285807, 3, 167, 255],
    [-0.025151, -0.285807, 0, 215, 232],
    [-0.130328, -0.244651, 3, 167, 255],
    [0.139473, -0.244651, 0, 215, 232],
    [-0.258370, -0.235506, 3, 167, 255],
    [0.066307, -0.230932, 0, 215, 232],
    [0.121182, -0.230932, 0, 215, 232],
    [0.084599, -0.212640, 0, 215, 232],
    [-0.354402, -0.185203, 11, 111, 255],
    [-0.322391, -0.166912, 11, 111, 255],
    [-0.313245, -0.080026, 11, 111, 255],
    [0.326963, -0.038870, 0, 215, 232],
    [-0.299526, 0.029724, 56, 65, 255],
    [-0.066308, 0.326963, 56, 65, 255],
    [-0.052589, -0.331537, 0, 215, 232],
    [0.048016, -0.203495, 0, 215, 232],
    [-0.395557, 0.002286, 56, 65, 255],
    [0.020579, 0.002286, 11, 111, 255],
    [0.189775, 0.052588, 3, 167, 255],
    [0.166911, 0.057161, 11, 111, 255],
    [0.084599, 0.070880, 3, 167, 255],
    [0.084599, 0.098318, 3, 167, 255],
    [0.029724, 0.107463, 3, 167, 255],
    [0.358974, 0.121182, 3, 167, 255],
    [0.144047, 0.125755, 11, 111, 255],
    [0.089172, 0.134900, 11, 111, 255],
    [0.139474, 0.144047, 3, 167, 255],
    [0.061734, 0.166911, 11, 111, 255],
    [-0.358975, 0.185204, 76, 25, 214],
    [-0.304100, 0.194349, 56, 65, 255],
    [0.258369, 0.203494, 11, 111, 255],
    [0.134900, 0.240079, 11, 111, 255],
    [0.052588, 0.249224, 11, 111, 255],
    [-0.162339, 0.258369, 76, 25, 214],
    [-0.244651, 0.262943, 56, 65, 255],
    [0.057161, 0.345255, 11, 111, 255],
    [0.084599, 0.345255, 11, 111, 255],
    [-0.322390, 0.166911, 56, 65, 255],
    [-0.025151, 0.299525, 11, 111, 255],
    [0.230932, -0.006859, 3, 167, 255],
    [-0.025150, -0.331537, 0, 215, 232],
    [0.112036, -0.249225, 0, 215, 232],
    [0.267516, 0.034297, 0, 215, 232],
    [-0.098318, 0.061735, 11, 111, 255],
    [-0.208068, 0.130329, 56, 65, 255],
    [-0.084600, 0.180630, 56, 65, 255],
    [-0.258370, -0.208068, 11, 111, 255],
    [-0.313245, -0.098318, 11, 111, 255],
    [-0.267516, 0.093744, 56, 65, 255],
    [-0.267516, 0.121182, 56, 65, 255],
    [-0.390984, -0.066307, 11, 111, 255],
    [-0.354401, 0.084599, 76, 25, 214],
    [-0.249224, 0.098318, 56, 65, 255],
    [-0.226359, 0.125755, 56, 65, 255],
    [0.002286, 0.148619, 56, 65, 255],
    [-0.171484, 0.162338, 56, 65, 255],
    [-0.185203, 0.208068, 56, 65, 255],
    [-0.244650, 0.217213, 56, 65, 255],
    [-0.048016, 0.326963, 56, 65, 255],
    [0.116609, 0.052588, 3, 167, 255],
    [0.153193, 0.322391, 11, 111, 255],
    [0.107463, 0.336110, 56, 65, 255],
    [0.390984, -0.011432, 0, 215, 232],
    [0.267515, -0.038870, 0, 215, 232],
    [0.404703, 0.061735, 0, 215, 232],
    [0.011432, 0.102891, 11, 111, 255],
    [0.358973, -0.025151, 0, 215, 232],
    [0.176057, 0.080025, 3, 167, 255],
    [0.322391, 0.185204, 3, 167, 255],
    [-0.052589, 0.217213, 11, 111, 255],
    [-0.189776, 0.276661, 76, 25, 214],
    [-0.125756, 0.304099, 76, 25, 214],
    [-0.345256, -0.212640, 11, 111, 255],
    [-0.016005, 0.048016, 11, 111, 255],
    [-0.034296, 0.176057, 56, 65, 255],
    [-0.011432, 0.272088, 56, 65, 255],
    [-0.057161, 0.354400, 56, 65, 255],
    [-0.208068, 0.281235, 56, 65, 255],
    [0.322391, -0.002287, 0, 215, 232],
    [0.354402, 0.006860, 0, 215, 232],
    [0.034297, 0.034297, 11, 111, 255],
    [-0.006859, 0.066307, 11, 111, 255],
    [0.121182, 0.112036, 11, 111, 255],
    [-0.153193, 0.116610, 11, 111, 255],
    [0.116610, 0.134900, 11, 111, 255],
    [0.276661, 0.185204, 11, 111, 255],
    [-0.285807, 0.217213, 76, 25, 214],
    [0.235505, 0.226360, 11, 111, 255],
    [-0.048016, 0.308672, 56, 65, 255],
    [0.176058, 0.313244, 11, 111, 255],
    [-0.180631, 0.336110, 56, 65, 255],
    [-0.313244, -0.221787, 11, 111, 255],
    [-0.372693, 0.048016, 76, 25, 214],
    [-0.107464, -0.363546, 3, 167, 255],
    [-0.006859, -0.304100, 0, 215, 232],
    [-0.226359, -0.285807, 3, 167, 255],
    [-0.057162, -0.267515, 3, 167, 255],
    [-0.070881, -0.253796, 3, 167, 255],
    [-0.180631, -0.221787, 3, 167, 255],
    [-0.212640, -0.217214, 3, 167, 255],
    [-0.313245, -0.194350, 11, 111, 255],
    [-0.276662, -0.189776, 11, 111, 255],
    [0.070880, -0.185203, 3, 167, 255],
    [-0.322390, -0.148620, 11, 111, 255],
    [-0.368120, -0.130328, 56, 65, 255],
    [-0.304100, -0.125756, 11, 111, 255],
    [-0.276662, -0.121182, 11, 111, 255],
    [-0.363546, -0.084600, 11, 111, 255],
    [-0.390984, -0.029725, 56, 65, 255],
    [0.244650, -0.029725, 3, 167, 255],
    [-0.075453, 0.070880, 11, 111, 255],
    [-0.208068, 0.084599, 56, 65, 255],
    [-0.139475, 0.102891, 11, 111, 255],
    [-0.358975, 0.116610, 76, 25, 214],
    [-0.025150, 0.125755, 11, 111, 255],
    [-0.349828, 0.166911, 56, 65, 255],
    [-0.235506, 0.244650, 56, 65, 255],
    [-0.098317, 0.253797, 56, 65, 255],
    [-0.226359, 0.272088, 56, 65, 255],
    [0.020578, -0.340682, 0, 215, 232],
    [0.130328, 0.016005, 3, 167, 255],
    [0.395558, 0.043443, 0, 215, 232],
    [0.011433, 0.166911, 11, 111, 255],
    [-0.112037, 0.144047, 56, 65, 255],
    [-0.194349, 0.180630, 56, 65, 255],
    [-0.349828, -0.162339, 11, 111, 255],
    [-0.322390, 0.093744, 56, 65, 255],
    [-0.285807, 0.112036, 56, 65, 255],
    [-0.258370, 0.189775, 56, 65, 255],
    [-0.157765, -0.349828, 3, 167, 255],
    [0.002287, -0.322390, 0, 215, 232],
    [-0.235505, -0.304100, 3, 167, 255],
    [0.093745, -0.230932, 0, 215, 232],
    [0.180631, -0.006859, 3, 167, 255],
    [-0.299525, 0.162338, 56, 65, 255],
    [-0.144046, 0.313244, 76, 25, 214],
    [0.134900, 0.331536, 11, 111, 255],
    [0.157766, 0.038869, 3, 167, 255],
    [-0.038870, 0.253797, 56, 65, 255],
    [-0.153193, -0.235506, 11, 111, 255],
    [-0.157765, 0.066307, 11, 111, 255],
    [-0.116609, 0.258369, 56, 65, 255],
    [-0.153192, 0.157766, 56, 65, 255],
    [-0.390985, 0.070880, 56, 65, 255],
    [-0.349828, 0.139474, 56, 65, 255],
    [-0.331536, 0.194349, 76, 25, 214],
    [-0.198922, 0.066307, 56, 65, 255],
    [-0.134902, 0.217213, 56, 65, 255],
    [0.244652, -0.057162, 0, 215, 232],
    [0.048016, 0.125755, 3, 167, 255],
    [-0.198922, 0.116610, 56, 65, 255],
    [-0.020578, 0.317818, 56, 65, 255],
    [-0.057161, 0.057161, 11, 111, 255],
    [0.340683, -0.016006, 0, 215, 232],
    [0.212640, 0.075454, 3, 167, 255],
    [0.020579, 0.130329, 11, 111, 255],
    [-0.075454, 0.313244, 56, 65, 255],
    [-0.006859, -0.349828, 0, 215, 232],
    [0.075453, 0.016005, 3, 167, 255],
    [-0.034297, 0.107463, 11, 111, 255],
    [-0.052589, 0.244650, 11, 111, 255],
    [-0.162339, 0.272088, 56, 65, 255],
    [0.002286, 0.308672, 11, 111, 255],
    [-0.034296, 0.331536, 56, 65, 255],
    [-0.016005, 0.354400, 56, 65, 255],
    [-0.299525, 0.002286, 56, 65, 255],
    [-0.102890, -0.331537, 3, 167, 255],
    [-0.281234, -0.276662, 3, 167, 255],
    [-0.253796, -0.272089, 11, 111, 255],
    [-0.029725, -0.267515, 3, 167, 255],
    [-0.112037, -0.262943, 3, 167, 255],
    [0.070880, -0.262943, 0, 215, 232],
    [-0.240078, -0.258370, 11, 111, 255],
    [-0.043443, -0.249225, 0, 215, 232],
    [0.020579, -0.244651, 3, 167, 255],
    [-0.180631, -0.235506, 11, 111, 255],
    [0.130329, -0.212640, 0, 215, 232],
    [-0.294953, -0.198921, 11, 111, 255],
    [0.061735, -0.198921, 0, 215, 232],
    [-0.326964, -0.080026, 56, 65, 255],
    [-0.299526, -0.070881, 56, 65, 255],
    [-0.372693, -0.066307, 11, 111, 255],
    [-0.326964, -0.052589, 56, 65, 255],
    [0.281235, -0.048015, 0, 215, 232],
    [0.294954, -0.034296, 3, 167, 255],
    [0.189775, -0.025151, 0, 215, 232],
    [0.230932, -0.020578, 0, 215, 232],
    [0.276661, -0.020578, 0, 215, 232],
    [-0.313245, -0.011432, 56, 65, 255],
    [0.258369, -0.011432, 0, 215, 232],
    [-0.349828, 0.006860, 56, 65, 255],
    [0.221786, 0.006860, 0, 215, 232],
    [0.400130, 0.016005, 0, 215, 232],
    [-0.020578, 0.034297, 11, 111, 255],
    [0.052588, 0.043443, 11, 111, 255],
    [-0.130328, 0.061735, 11, 111, 255],
    [-0.363547, 0.070880, 56, 65, 255],
    [-0.240078, 0.070880, 56, 65, 255],
    [-0.308671, 0.075454, 76, 25, 214],
    [-0.121182, 0.075454, 11, 111, 255],
    [-0.377265, 0.098318, 76, 25, 214],
    [-0.020579, 0.098318, 11, 111, 255],
    [-0.326964, 0.121182, 76, 25, 214],
    [-0.125756, 0.121182, 56, 65, 255],
    [0.089172, 0.148619, 11, 111, 255],
    [0.057162, 0.180630, 11, 111, 255],
    [-0.084600, 0.208068, 56, 65, 255],
    [0.070880, 0.226360, 11, 111, 255],
    [-0.025151, 0.230932, 56, 65, 255],
    [-0.258370, 0.235505, 76, 25, 214],
    [-0.157766, 0.244650, 56, 65, 255],
    [-0.011432, 0.244650, 56, 65, 255],
    [-0.272089, 0.272088, 56, 65, 255],
    [-0.144046, 0.349829, 76, 25, 214],
    [0.020578, 0.368119, 56, 65, 255],
    [-0.340682, -0.038870, 56, 65, 255],
    [-0.331537, 0.226360, 76, 25, 214],
    [-0.317818, 0.212641, 76, 25, 214],
    [0.057162, -0.249225, 0, 215, 232],
    [-0.066307, 0.340682, 56, 65, 255],
    [-0.171484, 0.230932, 76, 25, 214],
    [-0.194348, -0.240078, 3, 167, 255],
    [-0.281234, -0.139475, 11, 111, 255],
    [-0.294953, -0.116609, 11, 111, 255],
    [0.107463, -0.198921, 0, 215, 232],
    [-0.043442, 0.349829, 56, 65, 255],
    [-0.386412, 0.052588, 56, 65, 255],
    [0.377266, 0.093744, 3, 167, 255],
    [0.240079, 0.148619, 11, 111, 255],
    [0.267516, 0.253797, 3, 167, 255],
    [0.253797, 0.267516, 3, 167, 255],
];

};
function load(id){id=id.replace(/^\.\//,'').replace(/\.js$/,'');if(cache[id])return cache[id];if(!factories[id])throw Error('Missing core module');const e=cache[id]={};factories[id](e,load);return e;}
global.PetNative=load('pet-native').PetNative;
})(globalThis);
