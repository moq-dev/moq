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

// libopus 1.6.1's silk_resampler_private_down_FIR and silk_VAD_GetSA_Q8_c on the same samples.
// biome-ignore format: one row per half second
const LIBOPUS_48K_20MS = [
	2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,3,2,2,4,2,2,2,2,2,
	207,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,
	229,250,251,250,250,250,250,250,249,249,249,248,248,247,247,247,246,246,245,244,244,244,243,242,241,
	126,2,2,2,2,2,2,2,2,4,2,3,2,3,3,9,5,2,2,2,9,48,50,10,2,
];

// biome-ignore format: one row per half second
const LIBOPUS_24K_10MS = [
	2,2,2,2,2,2,2,2,2,5,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,3,2,2,2,2,4,2,2,3,2,2,2,2,2,2,2,2,2,
	172,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,182,
	239,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,254,253,251,249,247,246,
	112,2,2,2,2,2,2,2,2,2,2,2,2,3,2,2,2,4,2,2,2,2,10,2,2,2,2,2,2,10,10,7,4,3,4,4,2,2,2,2,3,44,69,32,67,41,11,10,2,2,
];

describe("Vad", () => {
	test("scores 48 kHz in 20 ms blocks exactly as libopus does", () => {
		expect(scores(new Vad({ sampleRate: 48_000, block: 20 }), signal(48_000), 48_000)).toEqual(LIBOPUS_48K_20MS);
	});

	test("scores 24 kHz in 10 ms blocks exactly as libopus does", () => {
		expect(scores(new Vad({ sampleRate: 24_000, block: 10 }), signal(24_000), 24_000)).toEqual(LIBOPUS_24K_10MS);
	});

	test("a rate without an integer path to SILK's still scores", () => {
		const vad = new Vad({ sampleRate: 44_100, block: 20 });
		const active = scores(vad, signal(44_100), 44_100).map((q8) => q8 >= 13);
		expect(active.slice(0, 25).some(Boolean)).toBe(false);
		expect(active.slice(26, 74).every(Boolean)).toBe(true);
	});

	test("push reports whether any block it completes is speech", () => {
		const vad = new Vad({ sampleRate: 48_000, block: 20 });
		const samples = signal(48_000);
		// Half a block completes nothing.
		expect(vad.push(chunk(samples, 0, 480, 48_000))).toBeUndefined();
		expect(vad.push(chunk(samples, 480, 24_000, 48_000))).toBe(false);
		// The room tone's last blocks and the burst's first.
		expect(vad.push(chunk(samples, 24_000, 26_880, 48_000))).toBe(true);
	});
});
