import { describe, expect, it } from "bun:test";
import { Duration, Instant } from "@moq/clock";
import { Manual } from "@moq/clock/test";
import * as Container from "@moq/hang/container";
import { Time, Track, Varint } from "@moq/net";
import { Signal } from "@moq/signals";
import { STRETCH } from "../sync";
import { maxDelay, target } from "./latency";
import { AudioRingBuffer } from "./ring-buffer";

// Replay recorded-shape arrival traces through the ring and count what a listener would hear:
// quanta rendered short (an underrun) and samples the ring threw away (a skip). The ring is sized
// from the target the estimator settles on, measured through the transport subscription and
// container consumer the decoder reads.

const RATE = 48000;
const QUANTUM = 128; // an AudioWorklet render quantum
const CHUNK_MS = 20; // one Opus frame
const CHUNK = (RATE * CHUNK_MS) / 1000;

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

/** A legacy container frame: the media timestamp, then a payload the consumer never decodes. */
function encode(media: number): Uint8Array {
	const timestamp = Varint.encode(media);
	const frame = new Uint8Array(timestamp.byteLength + 1);
	frame.set(timestamp, 0);
	return frame;
}

interface Measured {
	/** The playout target after each distinct arrival instant, in milliseconds. */
	targets: number[];
	/** Frames that reached the consumer's reader rather than being dropped for age. */
	delivered: number;
}

/**
 * Feed `t` through the path the decoder measures on: a transport subscription into a container
 * consumer, on a Manual clock, one group per frame. After each arrival `budget` turns the
 * current target into the subscription's max delay, the way the decoder does.
 */
async function measure(t: Arrival[], budget: (target: Duration) => Duration): Promise<Measured> {
	const clock = new Manual();
	const installed = clock.install();
	const track = new Track.Producer("audio");
	// One budget for both, as the decoder keeps: the container skips at the max delay it subscribes with.
	const delay = new Signal<Track.Delay>({ max: budget(Duration.ZERO) });
	const subscriber = track.subscribe({ delay: delay.peek() });
	const consumer = new Container.Consumer(subscriber, {
		format: new Container.Legacy.Format("audio"),
		delay,
	});

	const result: Measured = { targets: [], delivered: 0 };
	const reader = (async () => {
		for (;;) {
			const next = await consumer.next();
			if (!next) return;
			if (next.kind === "frame") result.delivered++;
		}
	})();

	try {
		let next = 0;
		while (next < t.length) {
			const arrival = t[next].arrival;
			clock.jump(Duration.millis(arrival).saturatingSub(clock.now().since(Instant.ZERO)));
			for (; next < t.length && t[next].arrival === arrival; next++) {
				const media = t[next].media * 1000;
				const group = track.createGroup(BigInt(next));
				group.writeFrame({ payload: encode(media), timestamp: Time.Timestamp.fromMicros(media) });
				group.finish();
			}

			// A macrotask boundary, so the group readers have observed everything written above.
			await new Promise((resolve) => setImmediate(resolve));

			const current = target({ measured: consumer.spread.peek(), frame: Duration.millis(CHUNK_MS) });
			result.targets.push(current.asMillis());
			delay.set({ max: budget(current) });
			subscriber.update({ delay: delay.peek() });
		}
	} finally {
		track.finish();
		track.close();
		consumer.close();
		await reader;
		installed.close();
	}

	return result;
}

/** The subscription "auto" asks for: the target plus the estimator's 2 s ceiling. */
const reached = (target: Duration) => maxDelay({ delay: target, maxDelay: target });

/** The target the estimator settles on by the end of `t`, given the subscription "auto" asks for. */
async function settled(t: Arrival[]): Promise<number> {
	const { targets } = await measure(t, reached);
	return targets[targets.length - 1];
}

/** The worklet's ring at `latencyMs`, and the clock it plays onto. */
interface Built {
	ring: AudioRingBuffer;
	latency: number;
}

function build(latency: number): Built {
	return { ring: new AudioRingBuffer({ rate: RATE, channels: 1, latency, bound: STRETCH.asMillis() }), latency };
}

interface Result {
	/** Quanta rendered short of a full block after the warmup, i.e. audible underruns. */
	underruns: number;
	/** Times the ring re-anchored after the warmup, dropping buffered audio rather than playing it. */
	skips: number;
	/** Samples time stretch added or removed after the warmup: output against the media it covered. */
	stretched: number;
}

/**
 * Play `t` through `ring` in real time: insert every frame the moment it arrives, and pull one
 * render quantum every quantum's worth of wall time, which is what the AudioWorklet does. The shared
 * clock trails the trace's 50 ms network delay by the latency.
 */
function replay({ ring, latency }: Built, t: Arrival[], warmupMs: number): Result {
	const step = (QUANTUM / RATE) * 1000;
	const output = [new Float32Array(QUANTUM)];
	const end = t[t.length - 1].arrival;
	const position = () => (ring.timestamp * RATE) / 1_000_000;

	let next = 0;
	let short = 0;
	let skips = 0;
	let stretched = 0;
	let started = false;

	for (let now = 0; now < end; now += step) {
		const playhead = position();
		const skipped = ring.skips;
		while (next < t.length && t[next].arrival <= now) {
			ring.write(t[next].media * 1000, [new Float32Array(CHUNK).fill(0.5)]);
			next++;
		}

		ring.clock((now - 50 - latency) * 1000);
		const count = ring.read(output);
		if (now < warmupMs) {
			if (count > 0) started = true;
			continue;
		}
		if (started && count < QUANTUM) short++;
		if (count > 0) started = true;
		if (ring.skips > skipped) skips += ring.skips - skipped;
		else stretched += Math.abs(position() - playhead - count);
	}

	return { underruns: short, skips, stretched: Math.round(stretched) };
}

describe("ring replay", () => {
	it("plays an evenly paced sender without underruns or skips", async () => {
		const t = trace(600, 1, 10);
		expect(replay(build(await settled(t)), t, 2000)).toMatchObject({ underruns: 0, skips: 0 });
	});

	it("plays a bursty sender at the measured target", async () => {
		// Three frames flushed at once, which is what an importer packing PES payloads produces.
		const t = trace(600, 3, 5);
		const measured = await settled(t);
		expect(measured).toBeGreaterThan(2 * CHUNK_MS);
		expect(replay(build(measured), t, 2000)).toMatchObject({ underruns: 0, skips: 0 });
	});

	it("keeps a five frame flush span playing", async () => {
		// A hundred milliseconds of arrivals landing at once. The target covers the trough and the
		// slack above it absorbs the peak, so the flush plays instead of being cut down to the
		// target on arrival.
		const t = trace(600, 5, 5);
		expect(replay(build(await settled(t)), t, 2000)).toMatchObject({ underruns: 0, skips: 0 });
	});

	it("trails the clock to bridge a target that ignores the arrival spread, dropping nothing", () => {
		// 46ms is what the round-trip formula produced on the connection this was measured on: it
		// describes the network and says nothing about a sender that flushes five frames at once.
		// Every flush's trough falls below it, so audio about to run out slows down, a tenth at most,
		// until it trails the clock by about the shortfall, inside the bound, and plays through every
		// flush rather than skipping or running dry on most of them.
		const t = trace(600, 5, 5);
		const result = replay(build(46), t, 2000);
		const flushes = (t.length - 2000 / CHUNK_MS) / 5;
		expect(result.underruns).toBeLessThan(flushes / 10);
		expect(result.skips).toBe(0);
		expect(result.stretched).toBeGreaterThan(0);
	});
});

describe("subscription budget", () => {
	// A minute of evenly paced audio, long enough to spend the prior and settle the target at its
	// floor, then six seconds of 100ms flushes.
	const PACED = 3000;
	const t: Arrival[] = [];
	for (let i = 0; i < PACED; i++) t.push({ media: i * CHUNK_MS, arrival: i * CHUNK_MS + 50 });
	t.push(
		...trace(300, 6, 0).map(({ media, arrival }) => ({
			media: media + PACED * CHUNK_MS,
			arrival: arrival + PACED * CHUNK_MS,
		})),
	);

	// The targets across the first twenty flushes, 2.4s of them.
	function onset(targets: number[]): number[] {
		return targets.slice(PACED, PACED + 20);
	}

	it("reaches a flush that starts after the target settled", async () => {
		const { targets, delivered } = await measure(t, reached);
		expect(targets[PACED - 1]).toBe(2 * CHUNK_MS);
		expect(delivered).toBe(t.length);
		// Every flush lands whole, so its 100ms reaches the 95th percentile within a few intervals.
		expect(Math.max(...onset(targets))).toBeGreaterThanOrEqual(120);
	});

	it("drops the flush it should measure when the budget follows the target", async () => {
		// The transport skips a group older than the subscription's max delay, before the consumer
		// can observe it, so the estimate only ever sees what the target it already holds allows.
		const { targets, delivered } = await measure(t, (target) => target);
		expect(delivered).toBeLessThan(t.length);
		expect(Math.max(...onset(targets))).toBeLessThan(120);
	});
});
