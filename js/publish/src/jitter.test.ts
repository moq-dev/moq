import { expect, spyOn, test } from "bun:test";
import { JitterClock, RenditionJitter } from "./jitter";

test("a batch flushed at its end counts its full media span", () => {
	const clock = new JitterClock();
	clock.observe(0, 0);
	expect(clock.observe(0, 120_000)).toBe(120_000);
	expect(clock.observe(40_000, 120_000)).toBe(80_000);
});

test("a constant lateness is not jitter", () => {
	const clock = new JitterClock();
	expect(clock.observe(0, 200_000)).toBe(0);
	expect(clock.observe(240_000, 440_000)).toBe(0);
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

test("each rendition measures against its own minimum", () => {
	const now = spyOn(performance, "now").mockReturnValue(0);
	try {
		const audio = new RenditionJitter();
		const video = new RenditionJitter();
		expect(audio.observe(0)).toBeUndefined();
		now.mockReturnValue(200);
		// A slower encoder's constant offset is not jitter.
		expect(video.observe(0)).toBeUndefined();
		now.mockReturnValue(300);
		expect(video.observe(40_000)).toBe(60);
		now.mockReturnValue(440);
		expect(video.observe(240_000)).toBeUndefined();
		expect(audio.current).toBeUndefined();
		expect(video.current).toBe(60);
	} finally {
		now.mockRestore();
	}
});
