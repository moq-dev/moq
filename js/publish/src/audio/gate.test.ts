import { describe, expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import type { AudioFrame } from "./capture";
import { Gate } from "./gate";

const RATE = 48_000;

// A seeded generator, so every run scores the same samples.
function noise(seed: number): () => number {
	let state = seed;
	return () => {
		state = (Math.imul(state, 1103515245) + 12345) | 0;
		return ((state >>> 8) / 0x800000 - 1) * 0.5;
	};
}

// Feeds a gate one codec frame at a time from the given start, in microseconds.
class Talker {
	readonly gate: Gate;
	readonly #frame: number;
	readonly #random = noise(7);
	#at: number;

	readonly #rate: number;

	constructor(frame = 20_000, rate = RATE) {
		this.#frame = frame;
		this.#rate = rate;
		this.#at = 0;
		this.gate = new Gate({ sampleRate: rate, frame: Duration.micros(frame) });
	}

	// Push `count` frames of loud noise (speech) or digital silence, returning the start of each frame passed.
	push(count: number, speech: boolean): number[] {
		const passed: number[] = [];
		for (let i = 0; i < count; i++) {
			const samples = new Float32Array((this.#rate * this.#frame) / 1_000_000);
			if (speech) for (let j = 0; j < samples.length; j++) samples[j] = this.#random();
			const frame: AudioFrame = { timestamp: Duration.micros(this.#at), channels: [samples] };
			this.#at += this.#frame;
			for (const out of this.gate.push(frame, Duration.micros(this.#at)))
				passed.push(out.frame.timestamp.asMicros());
		}
		return passed;
	}

	skip(micros: number): void {
		this.#at += micros;
	}
}

const ms = (start: number, count: number, step = 20) =>
	Array.from({ length: count }, (_, i) => (start + i * step) * 1000);

describe("Gate", () => {
	test("starts shut and sends speech with one frame of pre-roll", () => {
		const talker = new Talker();
		expect(talker.push(5, false)).toEqual([]);
		expect(talker.gate.open).toBe(false);
		// The frame before the speech goes first.
		expect(talker.push(3, true)).toEqual(ms(80, 4));
		expect(talker.gate.open).toBe(true);
	});

	test("shuts after 200 ms of silence, then reopens with pre-roll", () => {
		const talker = new Talker();
		talker.push(5, true);
		// Ten silent frames carry the tail, and the next shuts the gate.
		expect(talker.push(10, false)).toEqual(ms(100, 10));
		expect(talker.gate.open).toBe(true);
		expect(talker.push(1, false)).toEqual([]);
		expect(talker.gate.open).toBe(false);
		expect(talker.push(20, false)).toEqual([]);
		// Silence ends at 720 ms: the frame at 700 ms is the pre-roll.
		expect(talker.push(2, true)).toEqual(ms(700, 3));
	});

	test("speech inside the hangover keeps it open", () => {
		const talker = new Talker();
		talker.push(2, true);
		expect(talker.push(8, false)).toHaveLength(8);
		expect(talker.push(1, true)).toHaveLength(1);
		expect(talker.push(10, false)).toHaveLength(10);
		expect(talker.gate.open).toBe(true);
	});

	test("short frames pre-roll one 20 ms analysis step", () => {
		const talker = new Talker(5_000);
		expect(talker.push(8, false)).toEqual([]);
		expect(talker.push(2, true)).toEqual(ms(20, 6, 5));
	});

	test("a frame over 20 ms passes while any 20 ms of it is inside the hangover", () => {
		const talker = new Talker(60_000);
		talker.push(1, true);
		// The fourth silent frame's first 20 ms end 200 ms after the speech, so it is sent, and the
		// gate shuts behind it.
		expect(talker.push(4, false)).toEqual(ms(60, 4, 60));
		expect(talker.gate.open).toBe(false);
		expect(talker.push(1, false)).toEqual([]);
	});

	test("at 16 kHz, where Opus codes SILK alone, a frame over 20 ms is decided whole", () => {
		const talker = new Talker(60_000, 16_000);
		talker.push(1, true);
		expect(talker.push(3, false)).toEqual(ms(60, 3, 60));
		expect(talker.push(1, false)).toEqual([]);
	});

	test("below 16 kHz SILK decides, and short frames inherit its newest score", () => {
		const talker = new Talker(5_000, 12_000);
		expect(talker.push(8, false)).toEqual([]);
		// Speech from 40 ms is scored when its 10 ms block completes at 50 ms, so the 10 ms held
		// before that frame go first: the onset's own frame and the 5 ms before it.
		expect(talker.push(2, true)).toEqual(ms(35, 3, 5));
	});

	test("a gap in the timeline shuts the gate and drops the held pre-roll", () => {
		const talker = new Talker();
		talker.push(3, true);
		expect(talker.gate.open).toBe(true);
		talker.skip(1_000_000);
		// The frame after the gap is silent, so the gate shuts without the old hangover.
		expect(talker.push(1, false)).toEqual([]);
		expect(talker.gate.open).toBe(false);
		// The silent frame after the gap is pre-roll; nothing from before it is.
		expect(talker.push(1, true)).toEqual([1_060_000, 1_080_000]);
	});
});
