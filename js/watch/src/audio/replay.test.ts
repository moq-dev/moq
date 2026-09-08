import { describe, expect, it } from "bun:test";
import { Time } from "@moq/net";
import { AudioRingBuffer } from "./ring-buffer";
import { allocSharedRingBuffer, SharedRingBuffer } from "./shared-ring-buffer";

// Replay recorded-shape arrival traces through both rings and count what a listener would hear:
// quanta rendered short (an underrun) and samples the ring threw away (a skip). Both rings are
// driven through the same harness, since the postMessage fallback is the path every page without
// cross-origin isolation takes.

const RATE = 48000;
const QUANTUM = 128; // an AudioWorklet render quantum
const CHUNK_MS = 20; // one Opus frame
const CHUNK = (RATE * CHUNK_MS) / 1000;
// What the catalog advertises: one frame plus the render quantum. Sync holds this as a floor under
// the measured target.
const FLOOR = CHUNK_MS + Math.ceil((QUANTUM / RATE) * 1000);

/** Deterministic PRNG, so a trace is the same on every machine. */
function rng(seed: number): () => number {
	let a = seed >>> 0;
	return () => {
		a = (a + 0x6d2b79f5) >>> 0;
		let t = Math.imul(a ^ (a >>> 15), 1 | a);
		t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
	};
}

/** One frame of a trace: when it was captured, and the wall time it reached us. */
interface Arrival {
	media: number;
	arrival: number;
}

/**
 * A sender emitting a frame every `CHUNK_MS`, holding `burst - 1` of them back and flushing the
 * run at once, plus `spread` ms of per-flush network jitter. `burst` of 1 is an evenly paced
 * sender; anything above it is the PES packing an importer produces.
 */
function trace(frames: number, burst: number, spread: number, seed = 7): Arrival[] {
	const rand = rng(seed);
	const out: Arrival[] = [];
	let held: number[] = [];
	for (let i = 0; i < frames; i++) {
		const media = i * CHUNK_MS;
		held.push(media);
		if (held.length < burst) continue;
		// Every frame keeps its own capture time; the whole run shares one arrival.
		const arrival = media + 50 + rand() * spread;
		for (const m of held) out.push({ media: m, arrival });
		held = [];
	}
	return out;
}

/**
 * What the estimator converges on for a trace: the p95 arrival spread plus a frame of cushion,
 * held above the catalog floor.
 */
function target(t: Arrival[]): number {
	let min = Number.POSITIVE_INFINITY;
	for (const { media, arrival } of t) min = Math.min(min, arrival - media);
	const spreads = t.map(({ media, arrival }) => arrival - media - min).sort((a, b) => a - b);
	const p95 = Math.ceil(spreads[Math.floor(spreads.length * 0.95)]);
	return Math.max(FLOOR, p95 + CHUNK_MS);
}

/** The two rings behind one interface, since the harness drives them identically. */
interface Ring {
	insert(timestamp: Time.Micro, data: Float32Array[]): void;
	read(output: Float32Array[]): number;
	readonly length: number;
}

function shared(latencyMs: number): Ring {
	const samples = Math.ceil((RATE * latencyMs) / 1000);
	const ring = new SharedRingBuffer(allocSharedRingBuffer(1, Math.max(RATE, samples * 2), RATE));
	ring.setLatency(samples);
	return ring;
}

function post(latencyMs: number): Ring {
	const ring = new AudioRingBuffer({ rate: RATE, channels: 1, latency: latencyMs as Time.Milli });
	return {
		insert: (timestamp, data) => ring.write(timestamp, data),
		read: (output) => ring.read(output),
		get length() {
			return ring.length;
		},
	};
}

interface Result {
	/** Quanta rendered short of a full block after the warmup, i.e. audible underruns. */
	underruns: number;
	/** Samples inserted after the warmup that were never played: skipped or overflow-dropped. */
	skipped: number;
}

/**
 * Play `t` through `ring` in real time: insert every frame the moment it arrives, and pull one
 * render quantum every quantum's worth of wall time, which is what the AudioWorklet does.
 */
function replay(ring: Ring, t: Arrival[], warmupMs: number): Result {
	const step = (QUANTUM / RATE) * 1000;
	const output = [new Float32Array(QUANTUM)];
	const end = t[t.length - 1].arrival;

	let next = 0;
	let played = 0;
	let inserted = 0;
	let short = 0;
	let started = false;
	let mark: { played: number; inserted: number; buffered: number; short: number } | undefined;

	for (let now = 0; now < end; now += step) {
		while (next < t.length && t[next].arrival <= now) {
			ring.insert(Time.Micro.fromMilli(t[next].media as Time.Milli), [new Float32Array(CHUNK).fill(0.5)]);
			inserted += CHUNK;
			next++;
		}

		const count = ring.read(output);
		played += count;
		if (started && count < QUANTUM) short++;
		if (count > 0) started = true;

		if (now >= warmupMs && !mark) mark = { played, inserted, buffered: ring.length, short };
	}

	const from = mark ?? { played, inserted, buffered: ring.length, short };
	return {
		underruns: short - from.short,
		skipped: inserted - from.inserted - (played - from.played) - (ring.length - from.buffered),
	};
}

const RINGS: Array<[string, (latencyMs: number) => Ring]> = [
	["shared", shared],
	["post", post],
];

describe.each(RINGS)("%s ring replay", (_name, build) => {
	it("plays an evenly paced sender without underruns or skips", () => {
		const t = trace(600, 1, 10);
		expect(replay(build(target(t)), t, 2000)).toEqual({ underruns: 0, skipped: 0 });
	});

	it("plays a bursty sender at the measured target", () => {
		// Three frames flushed at once, which is what an importer packing PES payloads produces.
		const t = trace(600, 3, 5);
		expect(target(t)).toBeGreaterThan(2 * CHUNK_MS);
		const result = replay(build(target(t)), t, 2000);
		expect(result.underruns).toBeLessThanOrEqual(2);
		expect(result.skipped).toBe(0);
	});

	it("keeps a five frame flush span playing", () => {
		// A hundred milliseconds of arrivals landing at once. The target covers the trough and the
		// slack above it absorbs the peak, so the flush plays instead of being cut down to the
		// target on arrival.
		//
		// A 95th percentile target sits by construction at the edge of what arrives, so the tail
		// beyond it can still land a couple of times across the ten second window; closing that
		// without a deeper buffer is what the time-stretch quest is for. Two orders of magnitude
		// below the round-trip target below, and the postMessage ring bounds its refill at capacity
		// rather than in read(), so it can also land one flush past the band.
		const t = trace(600, 5, 5);
		const result = replay(build(target(t)), t, 2000);
		expect(result.underruns).toBeLessThanOrEqual(2);
		expect(result.skipped).toBeLessThanOrEqual(2 * CHUNK);
	});

	it("underruns constantly when the target ignores the arrival spread", () => {
		// 46ms is what the round-trip formula produced on the connection this was measured on: it
		// describes the network and says nothing about a sender that flushes five frames at once.
		const t = trace(600, 5, 5);
		const result = replay(build(46), t, 2000);
		expect(result.underruns).toBeGreaterThan(100);
		expect(result.skipped).toBeGreaterThan(10 * CHUNK);
	});
});
