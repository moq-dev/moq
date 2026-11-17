import { describe, expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import type { AudioFrame } from "./capture";
import { Vad } from "./vad";

// Two seconds of room tone, a loud burst, a steady two-tone, and a quiet talker fading in near the
// detector's threshold.
function signal(rate: number): Float32Array {
	let state = 11;
	const random = () => {
		state = (Math.imul(state, 1103515245) + 12345) | 0;
		return (state >>> 8) / 0x1000000 - 0.5;
	};
	return Float32Array.from({ length: rate * 2 }, (_, n) => {
		const t = n / rate;
		const room = random() * 0.002;
		if (t >= 0.5 && t < 1) return room + random() * 0.6 * Math.sin(Math.PI * (t - 0.5) * 2);
		if (t >= 1 && t < 1.5)
			return room + 0.05 * Math.sin(2 * Math.PI * 300 * t) + 0.02 * Math.sin(2 * Math.PI * 1700 * t);
		if (t >= 1.5) return room + 0.006 * (t - 1.5) * random() * Math.sin(2 * Math.PI * 4 * t);
		return room;
	});
}

// Samples [from, to) as a mono capture frame.
function chunk(samples: Float32Array, from: number, to: number, rate: number): AudioFrame {
	return { timestamp: Duration.micros((from / rate) * 1_000_000), channels: [samples.subarray(from, to)] };
}

// Every block's speech activity in Q8, fed in 128-sample capture quanta.
function scores(vad: Vad, samples: Float32Array, rate: number): number[] {
	const out: number[] = [];
	const activity = vad.activity.bind(vad);
	vad.activity = (block) => {
		const q8 = activity(block);
		out.push(q8);
		return q8;
	};
	for (let at = 0; at < samples.length; at += 128) vad.push(chunk(samples, at, at + 128, rate));
	return out;
}

// libopus 1.6.1's silk_VAD_GetSA_Q8_c on the same samples.
// biome-ignore format: one row per half second
const LIBOPUS_8K_20MS = [
	2,2,2,2,2,2,2,4,2,4,2,2,2,2,2,3,2,2,2,2,2,2,2,2,2,
	234,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,
	255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,
	166,2,2,2,2,2,2,2,2,2,2,2,2,2,2,4,13,2,2,2,10,20,41,5,2,
];

// biome-ignore format: one row per half second
const LIBOPUS_12K_10MS = [
	2,2,2,2,2,2,2,2,2,2,3,4,3,3,2,2,2,2,2,2,5,4,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,3,3,2,2,2,2,2,6,
	185,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,228,
	241,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,254,254,253,251,249,248,244,242,
	101,2,2,2,2,2,2,4,2,2,7,2,2,4,2,2,2,2,2,2,2,10,4,3,2,2,2,2,2,8,26,34,74,28,2,2,2,2,2,2,3,16,50,58,103,87,81,6,2,2,
];

describe("Vad", () => {
	test("scores 8 kHz in 20 ms blocks exactly as libopus does", () => {
		expect(scores(new Vad({ sampleRate: 8000, block: 20 }), signal(8000), 8000)).toEqual(LIBOPUS_8K_20MS);
	});

	test("scores 12 kHz in 10 ms blocks exactly as libopus does", () => {
		expect(scores(new Vad({ sampleRate: 12_000, block: 10 }), signal(12_000), 12_000)).toEqual(LIBOPUS_12K_10MS);
	});

	test("push reports whether any block it completes is speech", () => {
		const vad = new Vad({ sampleRate: 8000, block: 20 });
		const samples = signal(8000);
		// Half a block completes nothing.
		expect(vad.push(chunk(samples, 0, 80, 8000))).toBeUndefined();
		expect(vad.push(chunk(samples, 80, 4000, 8000))).toBe(false);
		// The room tone's last blocks and the burst's first.
		expect(vad.push(chunk(samples, 4000, 4480, 8000))).toBe(true);
	});
});
