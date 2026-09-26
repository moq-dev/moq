import { expect, test } from "bun:test";
import { u53 } from "@moq/hang/catalog";
import { Baseline, Estimator } from "./jitter";

test("a batch flushed at its end counts its full media span", () => {
	const broadcast = new Baseline();
	const estimator = new Estimator();
	estimator.flush(0, broadcast, 0);
	expect(estimator.flush(0, broadcast, 120_000)).toBe(true);
	expect(estimator.estimate.jitter).toBe(u53(120));
	expect(estimator.flush(40_000, broadcast, 120_000)).toBe(false);
});

test("a constant lateness is not jitter", () => {
	const broadcast = new Baseline();
	const estimator = new Estimator();
	expect(estimator.flush(0, broadcast, 200_000)).toBe(false);
	expect(estimator.flush(240_000, broadcast, 440_000)).toBe(false);
	expect(estimator.estimate).toEqual({ jitter: undefined, delay: undefined });
});

test("a sliding minimum bounds slow media clock drift", () => {
	const broadcast = new Baseline();
	const estimator = new Estimator();
	for (let second = 0; second < 100; second++) {
		estimator.flush(second * 1_000_000, broadcast, second * 1_001_000);
	}
	expect(estimator.estimate.jitter).toBeLessThanOrEqual(10);
	expect(estimator.estimate.jitter).toBeGreaterThan(0);
});

test("a faster-than-real-time source keeps lowering the baseline", () => {
	const broadcast = new Baseline();
	const estimator = new Estimator();
	for (let second = 0; second < 100; second++) {
		expect(estimator.flush(second * 2_000_000, broadcast, second * 1_000_000)).toBe(false);
	}
});

test("each rendition measures jitter against its own minimum", () => {
	const broadcast = new Baseline();
	const audio = new Estimator();
	const video = new Estimator();
	audio.flush(0, broadcast, 0);
	// A slower encoder's constant offset is not jitter.
	video.flush(0, broadcast, 200_000);
	video.flush(40_000, broadcast, 300_000);
	video.flush(240_000, broadcast, 440_000);
	expect(audio.estimate.jitter).toBeUndefined();
	expect(video.estimate.jitter).toBe(u53(60));
});

test("a rendition trailing the broadcast's earliest advertises the gap as delay", () => {
	const broadcast = new Baseline();
	const audio = new Estimator();
	const video = new Estimator();
	for (let frame = 0; frame < 10; frame++) {
		const timestamp = frame * 20_000;
		audio.flush(timestamp, broadcast, timestamp + 5_000);
		video.flush(timestamp, broadcast, timestamp + 205_000);
	}
	expect(audio.estimate).toEqual({ jitter: undefined, delay: undefined });
	expect(video.estimate).toEqual({ jitter: undefined, delay: u53(200) });
});

test("delay is a lifetime maximum", () => {
	const broadcast = new Baseline();
	const audio = new Estimator();
	const video = new Estimator();
	audio.flush(0, broadcast, 0);
	expect(video.flush(0, broadcast, 150_000)).toBe(true);
	expect(video.estimate.delay).toBe(u53(150));

	// Video catches up, then audio stops long enough to leave the window: neither lowers it.
	expect(video.flush(100_000, broadcast, 100_000)).toBe(false);
	expect(video.flush(20_000_000, broadcast, 20_000_000)).toBe(false);
	expect(video.estimate.delay).toBe(u53(150));
});

test("broadcasts do not share a baseline", () => {
	const audio = new Estimator();
	const video = new Estimator();
	audio.flush(0, new Baseline(), 0);
	video.flush(0, new Baseline(), 200_000);
	expect(video.estimate.delay).toBeUndefined();
});
