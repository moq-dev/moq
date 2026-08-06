import { describe, expect, test } from "bun:test";
import { Time } from "@moq/net";
import type { AudioFrame } from "./capture";
import { Framer } from "./framer";
import { Resampler } from "./resampler";

function quantum(sampleRate: number, index: number, origin = 0 as Time.Micro): AudioFrame {
	const timestamp = (origin + Time.Micro.fromSecond(((index * 128) / sampleRate) as Time.Second)) as Time.Micro;
	return {
		timestamp,
		channels: [Float32Array.from({ length: 128 }, (_, offset) => index * 128 + offset)],
	};
}

function opus(sampleRate: number, duration = 20_000 as Time.Micro): Framer {
	return new Framer({ sampleRate, channels: 1, size: { duration } });
}

describe("Opus frame alignment (#2387)", () => {
	test("collects 16kHz capture quanta into contiguous 20ms frames", () => {
		const framer = opus(16_000);
		const frames = Array.from({ length: 5 }, (_, index) => framer.push(quantum(16_000, index))).flat();

		expect(frames.map((frame) => frame.timestamp)).toEqual([Time.Micro(0), Time.Micro(20_000)]);
		expect(frames.map((frame) => frame.channels[0].length)).toEqual([320, 320]);
		expect(Array.from(frames[0].channels[0])).toEqual(Array.from({ length: 320 }, (_, index) => index));
		expect(Array.from(frames[1].channels[0])).toEqual(Array.from({ length: 320 }, (_, index) => index + 320));
	});

	test("collects 48kHz capture quanta into contiguous 20ms frames", () => {
		const origin = 123_456 as Time.Micro;
		const framer = opus(48_000);
		const frames = Array.from({ length: 15 }, (_, index) => framer.push(quantum(48_000, index, origin))).flat();

		expect(frames.map((frame) => frame.timestamp)).toEqual([origin, Time.Micro(origin + 20_000)]);
		expect(frames.map((frame) => frame.channels[0].length)).toEqual([960, 960]);
	});

	test("can emit frames shorter than a capture quantum", () => {
		const framer = opus(48_000, 2_500 as Time.Micro);
		const frames = [framer.push(quantum(48_000, 0)), framer.push(quantum(48_000, 1))].flat();

		expect(frames.map((frame) => frame.timestamp)).toEqual([Time.Micro(0), Time.Micro(2_500)]);
		expect(frames.map((frame) => frame.channels[0].length)).toEqual([120, 120]);
	});

	test("rounds cumulative boundaries without drifting at fractional sample counts", () => {
		const framer = opus(44_100, 5_000 as Time.Micro);
		const frames = Array.from({ length: 4 }, (_, index) => framer.push(quantum(44_100, index))).flat();

		expect(frames.map((frame) => frame.timestamp)).toEqual([Time.Micro(0), Time.Micro(5_000)]);
		expect(frames.map((frame) => frame.channels[0].length)).toEqual([221, 220]);
	});
});

test("collects fixed-size AAC frames", () => {
	const framer = new Framer({ sampleRate: 48_000, channels: 1, size: { samples: 1024 } });
	const frames = Array.from({ length: 8 }, (_, index) => framer.push(quantum(48_000, index))).flat();

	expect(frames).toHaveLength(1);
	expect(frames[0].timestamp).toBe(Time.Micro(0));
	expect(frames[0].channels[0]).toHaveLength(1024);
});

// A file source paces its audio against a shared timeline covering every track, so looping a file
// whose audio is shorter than its video leaves a gap at each wrap. Timestamps are counted forward
// from the first input, so without a resync the stream slides earlier by the gap and stays there,
// and the error compounds on every pass until audio and video are visibly apart.
describe("timestamp discontinuities (#2532)", () => {
	const RATE = 48_000;

	test("re-anchors after a gap instead of absorbing it", () => {
		const framer = opus(RATE);

		// One second of contiguous audio, then a wrap that lands 5s later on the wall clock.
		for (let index = 0; index < RATE / 128; index++) framer.push(quantum(RATE, index));

		const resumed = 6_000_000 as Time.Micro;
		const frames = [];
		for (let index = 0; index < RATE / 128; index++) {
			frames.push(...framer.push(quantum(RATE, index, resumed)));
		}

		// The first frame of the new segment starts where the input said it does, not one second in.
		expect(frames[0].timestamp).toBe(resumed);
		expect(frames[1].timestamp).toBe(Time.Micro(resumed + 20_000));
	});

	test("keeps counting through jitter below the threshold", () => {
		const framer = opus(RATE);

		// A sub-millisecond wobble is rounding, not a gap, so the timeline must not restart.
		for (let index = 0; index < RATE / 128; index++) framer.push(quantum(RATE, index));

		// A whole frame's worth, so a frame actually comes out: 48000 samples divides evenly into
		// 960-sample frames, leaving the buffer empty, and a short push would emit nothing at all.
		const nudged = (Time.Micro.fromSecond(1 as Time.Second) + 200) as Time.Micro;
		const frames = framer.push({
			timestamp: nudged,
			channels: [new Float32Array(960)],
		});

		// Still on the original timeline: the 51st 20ms frame, not a fresh origin at the nudge.
		expect(frames).toHaveLength(1);
		expect(frames[0].timestamp).toBe(Time.Micro(1_000_000));
	});

	test("drops only the partial frame at the seam", () => {
		const framer = opus(RATE);

		// Half of a 20ms frame, then a jump. Those buffered samples belong to the old timeline.
		framer.push({ timestamp: Time.Micro(0), channels: [new Float32Array(480)] });

		const resumed = 9_000_000 as Time.Micro;
		const frames = framer.push({ timestamp: resumed, channels: [new Float32Array(960)] });

		expect(frames.length).toBe(1);
		expect(frames[0].timestamp).toBe(resumed);
	});
});

// The composed path the file source actually uses: 44.1kHz decoded PCM, resampled to the 48kHz
// Opus needs, then framed into 20ms chunks. A file whose audio is shorter than its video leaves a
// gap at every loop wrap, and the gap has to survive both stages to re-anchor the encoded stream.
describe("resampled file audio across a loop wrap (#2532)", () => {
	const FILE_RATE = 44_100;
	const CODEC_RATE = 48_000;

	function chunk(timestamp: Time.Micro, samples: number): AudioFrame {
		return { timestamp, channels: [new Float32Array(samples)] };
	}

	test("re-anchors the encoded timeline after the wrap", () => {
		const resampler = new Resampler({ from: FILE_RATE, to: CODEC_RATE, channels: 1 });
		const framer = opus(CODEC_RATE);

		const push = (timestamp: Time.Micro, samples: number) => {
			const resampled = resampler.push(chunk(timestamp, samples));
			return resampled ? framer.push(resampled) : [];
		};

		// 1s of audio in 1024-sample packets, the shape an AAC file decodes into.
		const packet = 1024;
		const perPacket = Time.Micro.fromSecond((packet / FILE_RATE) as Time.Second);
		let index = 0;
		for (; index * packet < FILE_RATE; index++) {
			push((index * perPacket) as Time.Micro, packet);
		}

		// The wrap: audio ended early, so the next packet lands 2s later on the shared timeline.
		const resumed = (index * perPacket + 2_000_000) as Time.Micro;
		const frames: AudioFrame[] = [];
		for (let n = 0; n < 100; n++) {
			frames.push(...push((resumed + n * perPacket) as Time.Micro, packet));
		}

		// Encoded timestamps must follow the gap, not swallow it. Without the resync they would keep
		// counting from the pre-wrap origin and sit ~2s in the past forever, growing by another 2s
		// on every loop. The sub-millisecond slack is the resampler's first output sample, which
		// interpolates back across the chunk boundary and so lands just before it.
		expect(frames.length).toBeGreaterThan(0);
		expect(Math.abs(frames[0].timestamp - resumed)).toBeLessThan(1_000);

		// And they stay evenly spaced 20ms apart afterwards, to within float noise.
		for (let n = 1; n < frames.length; n++) {
			expect(frames[n].timestamp - frames[n - 1].timestamp).toBeCloseTo(20_000, 3);
		}
	});
});
