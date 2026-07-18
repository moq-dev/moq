import { describe, expect, test } from "bun:test";
import { Time } from "@moq/net";
import type { AudioFrame } from "./capture";
import { Framer } from "./framer";

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
