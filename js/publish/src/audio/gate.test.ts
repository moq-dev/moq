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

	constructor(frame = 20_000, start = 0) {
		this.#frame = frame;
		this.#at = start;
		this.gate = new Gate({ sampleRate: RATE, frame: Duration.micros(frame) });
	}

	// Push `count` frames of loud noise (speech) or digital silence, returning the start of each frame passed.
	push(count: number, speech: boolean): number[] {
		const passed: number[] = [];
		for (let i = 0; i < count; i++) {
			const samples = new Float32Array((RATE * this.#frame) / 1_000_000);
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
		// The first silent frame still scores as speech, since SILK counts the previous block's last
		// subframe again. Ten more carry the tail, and the next shuts the gate.
		expect(talker.push(11, false)).toEqual(ms(100, 11));
		expect(talker.gate.open).toBe(true);
		expect(talker.push(1, false)).toEqual([]);
		expect(talker.gate.open).toBe(false);
		expect(talker.push(20, false)).toEqual([]);
		// Silence ends at 740 ms: the frame at 720 ms is the pre-roll.
		expect(talker.push(2, true)).toEqual(ms(720, 3));
	});

	test("speech inside the hangover keeps it open", () => {
		const talker = new Talker();
		talker.push(2, true);
		expect(talker.push(8, false)).toHaveLength(8);
		expect(talker.push(1, true)).toHaveLength(1);
		expect(talker.push(10, false)).toHaveLength(10);
		expect(talker.gate.open).toBe(true);
	});

	test("short frames inherit the newest score and pre-roll one 10 ms block", () => {
		const talker = new Talker(5_000);
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

	test("stereo is scored on its mid channel", () => {
		const gate = new Gate({ sampleRate: RATE, frame: Duration.millis(20) });
		const random = noise(5);
		const left = Float32Array.from({ length: 960 }, random);
		// Out of phase: the mid channel is silent.
		const right = left.map((x) => -x);
		const frame = { timestamp: Duration.ZERO, channels: [left, right] };
		expect(gate.push(frame, Duration.millis(20))).toEqual([]);
	});
});
