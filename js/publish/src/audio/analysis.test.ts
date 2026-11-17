import { describe, expect, test } from "bun:test";
import { Duration } from "@moq/clock";
import { Analysis } from "./analysis";
import type { AudioFrame } from "./capture";
import { Gate } from "./gate";

// Five seconds: a voiced talker, room tone, a fan switched on at 2 s (about -50 dBFS, 20 dB over
// the room), and the talker back over the fan at 4 s. Quantized to 16 bits, as libopus took it.
function signal(rate: number): Float32Array {
	let state = 3;
	const random = () => {
		state = (Math.imul(state, 1103515245) + 12345) | 0;
		return (state >>> 8) / 0x1000000 - 0.5;
	};
	// A one-pole low-pass, for the fan's rumble.
	let fan = 0;
	let phase = 0;
	return Float32Array.from({ length: rate * 5 }, (_, n) => {
		const t = n / rate;
		const room = random() * 0.001;
		fan += 0.05 * (random() - fan);
		const rumble = t >= 2 ? fan * 0.07 : 0;
		// A voiced talker: harmonics of a gliding pitch, in syllables.
		phase += (2 * Math.PI * (130 + 25 * Math.sin(2 * Math.PI * 1.7 * t))) / rate;
		const syllable = Math.max(0, Math.sin(2 * Math.PI * 2.5 * t)) ** 2;
		let voice = 0;
		for (let h = 1; h <= 20; h++) voice += Math.sin(h * phase) / h;
		const talking = t < 1 || t >= 4;
		return Math.round((room + rumble + (talking ? 0.1 * syllable * voice : 0)) * 32767) / 32768;
	});
}

// Samples [from, to) as a capture frame of the given channels.
function chunk(samples: Float32Array, from: number, to: number, rate: number, channels = 1): AudioFrame {
	const plane = samples.subarray(from, to);
	return { timestamp: Duration.micros((from / rate) * 1_000_000), channels: Array(channels).fill(plane) };
}

function probabilities(rate: number, frame: number, channels = 1): (number | undefined)[] {
	const samples = signal(rate);
	const analysis = new Analysis({ sampleRate: rate });
	const out: (number | undefined)[] = [];
	for (let at = 0; at + frame <= samples.length; at += frame)
		out.push(analysis.probability(chunk(samples, at, at + frame, rate, channels)));
	return out;
}

// libopus 1.6.1's run_analysis and tonality_get_info on the same samples, per 20 ms frame at 48 kHz.
// biome-ignore format: one row per half second
const LIBOPUS_48K_20MS = [
	0.8182,0.3327,0.288,0.4999,0.1943,0.7671,0.9826,0.9957,0.9971,0.9937,0.9935,0.9901,0.9951,0.9981,0.9965,0.9906,0.9645,0.7813,0.4051,0.1588,0.71,0.9784,0.9974,0.9988,0.9993,
	0.9995,0.9995,0.9993,0.9996,0.9996,0.9991,0.9989,0.9994,0.9996,0.9989,0.9971,0.9916,0.9385,0.7438,0.3797,0.8674,0.9914,0.9984,0.9995,0.9995,0.9994,0.9997,0.9998,0.9998,0.9997,
	0.9995,0.9992,0.9996,0.9997,0.9988,0.997,0.9913,0.9349,0.736,0.4328,0.2175,0.1082,0.0689,0.0437,0.0316,0.0248,0.0218,0.0171,0.0148,0.013,0.0105,0.0086,0.0072,0.007,0.0058,
	0.0056,0.0049,0.0043,0.005,0.0049,0.0041,0.0033,0.0037,0.0035,0.0031,0.0038,0.0044,0.0036,0.0029,0.003,0.0042,0.0037,0.004,0.0032,0.0028,0.0025,0.0034,0.0034,0.0029,0.0029,
	0.8845,0.6886,0.5834,0.5155,0.6778,0.6534,0.7864,0.6319,0.5381,0.4244,0.3718,0.2828,0.2486,0.1984,0.1297,0.109,0.1073,0.1165,0.1195,0.1062,0.0734,0.0695,0.0442,0.035,0.0298,
	0.0281,0.0433,0.039,0.0263,0.02,0.0178,0.0301,0.0328,0.018,0.0309,0.0278,0.0285,0.0171,0.0291,0.0256,0.0166,0.0135,0.0132,0.0139,0.0117,0.0146,0.0106,0.0162,0.0144,0.0107,
	0.0084,0.0141,0.0341,0.0317,0.0186,0.0116,0.0097,0.0093,0.0075,0.0087,0.0124,0.0125,0.0161,0.0114,0.0083,0.0133,0.0133,0.0119,0.0081,0.0111,0.0071,0.0068,0.0104,0.0173,0.0169,
	0.0127,0.0101,0.0099,0.0089,0.0092,0.0091,0.0101,0.0099,0.0072,0.0079,0.0145,0.0128,0.0105,0.011,0.0078,0.0071,0.0145,0.0103,0.0136,0.0132,0.015,0.0074,0.0055,0.0049,0.0045,
	0.0176,0.8409,0.9774,0.99,0.9882,0.9855,0.9788,0.9904,0.9926,0.9844,0.9826,0.986,0.9939,0.9931,0.9887,0.984,0.9556,0.8364,0.7268,0.5763,0.3204,0.9375,0.9947,0.9988,0.999,
	0.9991,0.9993,0.9997,0.9998,0.9997,0.9994,0.9993,0.9996,0.9992,0.9966,0.9915,0.9478,0.7909,0.4993,0.2601,0.1447,0.8749,0.9944,0.9986,0.9984,0.9987,0.9986,0.9993,0.9989,0.9983,
];

// The same, per 60 ms frame at 16 kHz, which reads the second of each frame's three windows.
// biome-ignore format: one row per 1.26 seconds
const LIBOPUS_16K_60MS = [
	0.3362,0.223,0.9956,0.9929,0.9977,0.9583,0.1094,0.996,0.9993,0.9996,0.9988,0.9985,0.9115,0.5345,0.9993,0.9996,0.9997,0.9996,0.9966,0.7343,0.125,
	0.033,0.0129,0.0108,0.0064,0.0042,0.0042,0.0034,0.0036,0.0042,0.0041,0.0033,0.0038,0.643,0.3155,0.3541,0.0806,0.0215,0.0197,0.0134,0.0158,0.0094,
	0.0163,0.0064,0.0071,0.0078,0.0068,0.017,0.0068,0.0085,0.0061,0.0067,0.0075,0.0085,0.0041,0.004,0.0063,0.0048,0.0047,0.005,0.0038,0.0055,0.0043,
	0.0044,0.0032,0.0055,0.0045,0.9654,0.9793,0.986,0.9843,0.9851,0.4549,0.0419,0.9981,0.9992,0.9996,0.9994,0.9756,0.0706,0.8239,0.9988,0.9995,
];

// Which 20 ms frames the libopus 1.6.1 encoder sent no audio for (VoIP, voice, 32 kbps, complexity
// 9, DTX, float input): the room tone after the talker, and the fan from 0.6 s after it starts.
const LIBOPUS_48K_DTX =
	"0000000000000000000000000000000000000000000000000000000000000000000000001111111111111111111111111111000000000000000000000000000000111111111111111111111111111111111111111111111111111111111111111111111100000000000000000000000000000000000000000000000000";

// The port runs in double precision where libopus runs in float.
function expectClose(actual: (number | undefined)[], expected: number[]): void {
	expect(actual).toHaveLength(expected.length);
	for (const [i, p] of actual.entries()) {
		expect(p).toBeDefined();
		expect(Math.abs((p as number) - expected[i])).toBeLessThan(1e-4);
	}
}

describe("Analysis", () => {
	test("scores 48 kHz in 20 ms frames as libopus does", () => {
		expectClose(probabilities(48_000, 960), LIBOPUS_48K_20MS);
	});

	test("scores 16 kHz in 60 ms frames as libopus does", () => {
		expectClose(probabilities(16_000, 960), LIBOPUS_16K_60MS);
	});

	test("stereo is analyzed on its downmix", () => {
		expectClose(probabilities(48_000, 960, 2), LIBOPUS_48K_20MS);
	});

	test("a rate libopus does not analyze at goes through 48 kHz", () => {
		const active = probabilities(44_100, 882).map((p) => (p ?? 1) >= 0.1);
		// The talker scores as active, and the fan as noise once learned.
		expect(active.slice(0, 50).filter(Boolean).length).toBeGreaterThan(45);
		expect(active.slice(130, 200).some(Boolean)).toBe(false);
	});

	test("a frame of no whole number of subframes reads the newest window, without drifting", () => {
		// The windows themselves, one per 20 ms.
		const windows = probabilities(48_000, 960);
		// AAC's 1024-sample frames are 21 1/3 ms.
		const samples = signal(48_000);
		const analysis = new Analysis({ sampleRate: 48_000 });
		for (let at = 0; at + 1024 <= samples.length; at += 1024) {
			const p = analysis.probability(chunk(samples, at, at + 1024, 48_000));
			const newest = Math.floor((at + 1024) / 960) - 1;
			expect([windows[newest], windows[newest - 1]]).toContain(p);
		}
	});

	test("a 10 ms frame before the first window has no score", () => {
		const analysis = new Analysis({ sampleRate: 48_000 });
		const samples = signal(48_000);
		expect(analysis.probability(chunk(samples, 0, 480, 48_000))).toBeUndefined();
		expect(analysis.probability(chunk(samples, 480, 960, 48_000))).toBeCloseTo(LIBOPUS_48K_20MS[0], 4);
	});

	test("digital silence is inactive", () => {
		const analysis = new Analysis({ sampleRate: 48_000 });
		const silence = new Float32Array(960);
		expect(analysis.push({ timestamp: Duration.ZERO, channels: [silence] })).toBe(false);
	});
});

describe("Gate", () => {
	test("sends what the libopus encoder sends, with one frame of pre-roll", () => {
		const samples = signal(48_000);
		const gate = new Gate({ sampleRate: 48_000, frame: Duration.millis(20) });
		const sent = new Set<number>();
		for (let i = 0; (i + 1) * 960 <= samples.length; i++) {
			for (const out of gate.push(chunk(samples, i * 960, (i + 1) * 960, 48_000), Duration.millis((i + 1) * 20)))
				sent.add(Math.round(out.frame.timestamp.asMillis() / 20));
		}
		const expected = new Set([...LIBOPUS_48K_DTX].flatMap((dtx, i) => (dtx === "0" ? [i] : [])));
		// Each reopening sends the frame before it too.
		for (const i of [...expected]) if (i > 0 && !expected.has(i - 1)) expected.add(i - 1);
		expect([...sent].sort((a, b) => a - b)).toEqual([...expected].sort((a, b) => a - b));
	});
});
