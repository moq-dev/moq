import { expect, spyOn, test } from "bun:test";
import type { Broadcast } from "./broadcast";
import { JitterClock, RenditionJitter } from "./jitter";

test("a batch flushed at its end counts its full media span", () => {
	const clock = new JitterClock();
	clock.observe(0, 0);
	expect(clock.observe(0, 120_000)).toBe(120_000);
	expect(clock.observe(40_000, 120_000)).toBe(80_000);
});

test("a shared baseline exposes the slower encoder's constant offset", () => {
	const clock = new JitterClock();
	expect(clock.observe(0, 0)).toBe(0);
	expect(clock.observe(0, 200_000)).toBe(200_000);
	expect(clock.observe(240_000, 240_000)).toBe(0);
	expect(clock.observe(240_000, 440_000)).toBe(200_000);
});

test("a sliding minimum bounds slow media clock drift", () => {
	const clock = new JitterClock();
	let maximum = 0;
	for (let second = 0; second < 100; second++) {
		maximum = Math.max(maximum, clock.observe(second * 1_000_000, second * 1_001_000));
	}
	expect(maximum).toBeLessThanOrEqual(10_000);
	expect(maximum).toBeGreaterThan(0);
});

test("a faster-than-real-time source keeps lowering the baseline", () => {
	const clock = new JitterClock();
	for (let second = 0; second < 100; second++) {
		expect(clock.observe(second * 2_000_000, second * 1_000_000)).toBe(0);
	}
});

test("each rendition retains its largest advertised flush delay", () => {
	const now = spyOn(performance, "now").mockReturnValue(0);
	try {
		const broadcast = {} as Broadcast;
		const audio = new RenditionJitter();
		const video = new RenditionJitter();
		expect(audio.observe(broadcast, 0)).toBeUndefined();
		now.mockReturnValue(200);
		expect(video.observe(broadcast, 0)).toBe(200);
		now.mockReturnValue(240);
		expect(audio.observe(broadcast, 240_000)).toBeUndefined();
		now.mockReturnValue(440);
		expect(video.observe(broadcast, 240_000)).toBeUndefined();
		expect(audio.current).toBeUndefined();
		expect(video.current).toBe(200);
	} finally {
		now.mockRestore();
	}
});
