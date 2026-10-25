import { expect, test } from "bun:test";
import { u53 } from "@moq/hang/catalog";
import { Estimator, Order } from "./order";

// Video at 20ms and audio at 15ms, written `v0 v20 a0 a15 v40 a30 v60 a45 a60`: audio's first frame
// lands after video has already written 20ms ahead of it.
test("the interleave measures the worst trail", () => {
	const order = new Order();
	const video = new Estimator();
	const audio = new Estimator();
	for (const [estimator, millis] of [
		[video, 0],
		[video, 20],
		[audio, 0],
		[audio, 15],
		[video, 40],
		[audio, 30],
		[video, 60],
		[audio, 45],
		[audio, 60],
	] as const) {
		estimator.write(millis * 1000, order);
	}
	expect(audio.estimate).toEqual({ delay: u53(20) });
	expect(video.estimate).toEqual({ delay: undefined });
});

test("a lone rendition never measures a delay", () => {
	const order = new Order();
	const video = new Estimator();
	for (const timestamp of [0, 120_000, 40_000, 80_000]) {
		expect(video.write(timestamp, order)).toBe(false);
	}
	expect(video.estimate.delay).toBeUndefined();
});

test("delay is a lifetime maximum", () => {
	const order = new Order();
	const audio = new Estimator();
	const video = new Estimator();
	audio.write(150_000, order);
	expect(video.write(0, order)).toBe(true);
	expect(video.estimate.delay).toBe(u53(150));

	// Video catches up and audio leaves: neither lowers it.
	expect(video.write(200_000, order)).toBe(false);
	order.leave(audio);
	expect(video.write(240_000, order)).toBe(false);
	expect(video.estimate.delay).toBe(u53(150));
});

test("a rendition that left no longer holds the others back", () => {
	const order = new Order();
	const ahead = new Estimator();
	const video = new Estimator();
	ahead.write(10_000_000, order);
	order.leave(ahead);
	expect(video.write(0, order)).toBe(false);
});

test("broadcasts do not share an order", () => {
	const audio = new Estimator();
	const video = new Estimator();
	audio.write(200_000, new Order());
	video.write(0, new Order());
	expect(video.estimate.delay).toBeUndefined();
});
