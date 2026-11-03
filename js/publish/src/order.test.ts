import { expect, test } from "bun:test";
import { u53 } from "@moq/hang/catalog";
import { Estimator, Order } from "./order";

// Video at 20ms and audio at 15ms, interleaved `v0 v20 a0 a15 v40 a30 v60 a45 a60` for two seconds:
// audio trails video by anything from nothing to 20ms, which varies frame to frame and so is no
// constant offset.
test("an interleave is not delay", () => {
	const order = new Order();
	const video = new Estimator();
	const audio = new Estimator();
	for (let cycle = 0; cycle < 34; cycle++) {
		const base = cycle * 60;
		for (const [estimator, millis] of [
			[video, 0],
			[video, 20],
			[audio, 0],
			[audio, 15],
			[video, 40],
			[audio, 30],
			[audio, 45],
		] as const) {
			estimator.write((base + millis) * 1000, order, true);
		}
	}
	expect(audio.estimate).toEqual({ delay: undefined });
	expect(video.estimate).toEqual({ delay: undefined });
});

// A rendition every frame of which trails keeps the least of those trails: the constant offset,
// without the lateness only some frames add. It is reported once a second of media has been measured.
test("a constant offset is delay, once a window closes", () => {
	const order = new Order();
	const video = new Estimator();
	const audio = new Estimator();
	let rose = 0;
	for (let i = 0; i < 100; i++) {
		video.write((100 + i * 20) * 1000, order, true);
		// Every third frame trails a further 10ms on top of the 100ms offset.
		const late = i % 3 === 1 ? 10 : 0;
		if (audio.write((i * 20 - late) * 1000, order, true)) rose++;
		if (i < 49) expect(audio.estimate.delay).toBeUndefined();
	}
	expect(rose).toBe(1);
	expect(audio.estimate).toEqual({ delay: u53(100) });
	expect(video.estimate).toEqual({ delay: undefined });
});

// Two renditions of one 50fps source with two B-frames (I0 P60 B20 B40 P120 B80 B100 ...) and audio,
// written in lockstep in decode order for three seconds. Renditions in `late` start at the third
// frame, as after a late join, so their first frame is a B-frame or audio trailing one. From
// moq-dev/moq#7579.
test("B-frame reorder is never delay, even from a late start", () => {
	for (const late of [[], [0, 2], [2], [0]]) {
		const order = new Order();
		const tracks = [new Estimator(), new Estimator(), new Estimator()];
		const decode = [0, 60, 20, 40];
		for (let gop = 1; gop < 50; gop++) decode.push(gop * 60 + 60, gop * 60 + 20, gop * 60 + 40);
		decode.forEach((pts, tick) => {
			[pts, pts, tick * 20].forEach((timestamp, index) => {
				if (tick < 2 && late.includes(index)) return;
				expect(tracks[index].write(timestamp * 1000, order, true)).toBe(false);
			});
		});
		for (const track of tracks) expect(track.estimate.delay).toBeUndefined();
	}
});

// Frames written before another rendition joins trail nothing, so the window they share with the
// first frames after it holds no trail.
test("a window spanning a join measures no delay", () => {
	const order = new Order();
	const audio = new Estimator();
	const video = new Estimator();
	for (let i = 0; i < 50; i++) audio.write(i * 20 * 1000, order, true);
	// A joining video's reference picture lands ahead of the next audio frame.
	video.write(1_040_000, order, true);
	expect(audio.write(1_000_000, order, true)).toBe(false);
	for (let i = 51; i < 150; i++) {
		video.write(i * 20 * 1000, order, true);
		expect(audio.write(i * 20 * 1000, order, true)).toBe(false);
	}
	expect(audio.estimate.delay).toBeUndefined();
});

// A window closes at a group boundary once a second has passed, so a group longer than a second is a
// window of its own: nothing is reported partway through it.
test("a window lasts a group when the group is longer than a second", () => {
	const order = new Order();
	const audio = new Estimator();
	const video = new Estimator();
	// Video in two-second groups, written 100ms behind the audio.
	for (let i = 0; i < 175; i++) {
		audio.write((100 + i * 20) * 1000, order, true);
		const rose = video.write(i * 20 * 1000, order, i % 100 === 0);
		if (i < 100) expect(video.estimate.delay).toBeUndefined();
		expect(rose).toBe(i === 100);
	}
	expect(video.estimate.delay).toBe(u53(100));
});

test("a lone rendition never measures a delay", () => {
	const order = new Order();
	const video = new Estimator();
	for (let gop = 0; gop < 10; gop++) {
		for (const millis of [0, 120, 40, 80]) {
			expect(video.write((gop * 160 + millis) * 1000, order, true)).toBe(false);
		}
	}
	expect(video.estimate.delay).toBeUndefined();
});

test("delay only rises", () => {
	const order = new Order();
	const audio = new Estimator();
	const video = new Estimator();
	for (let i = 0; i <= 51; i++) {
		audio.write((150 + i * 20) * 1000, order, true);
		video.write(i * 20 * 1000, order, true);
	}
	expect(video.estimate.delay).toBe(u53(150));

	// Video catches up and audio leaves: neither lowers it.
	for (let i = 0; i <= 50; i++) {
		audio.write((2_000 + i * 20) * 1000, order, true);
		video.write((2_000 + i * 20) * 1000, order, true);
	}
	order.leave(audio);
	for (let i = 0; i <= 50; i++) video.write((3_100 + i * 20) * 1000, order, true);
	expect(video.estimate.delay).toBe(u53(150));
});

// A rendition that resumes behind where it was trails by its new offset, however little it trailed
// before it left.
test("a rendition that leaves measures afresh", () => {
	const order = new Order();
	const video = new Estimator();
	const audio = new Estimator();
	for (let i = 0; i <= 60; i++) {
		video.write(i * 20 * 1000, order, true);
		audio.write(i * 20 * 1000, order, true);
	}
	order.leave(audio);
	for (let i = 0; i <= 60; i++) {
		video.write((2_000 + i * 20) * 1000, order, true);
		audio.write((1_800 + i * 20) * 1000, order, true);
	}
	expect(audio.estimate.delay).toBe(u53(200));
});

test("a rendition that left no longer holds the others back", () => {
	const order = new Order();
	const ahead = new Estimator();
	const other = new Estimator();
	const video = new Estimator();
	ahead.write(10_000_000, order, true);
	order.leave(ahead);
	for (let i = 0; i <= 60; i++) {
		other.write(i * 20 * 1000, order, true);
		expect(video.write(i * 20 * 1000, order, true)).toBe(false);
	}
	expect(video.estimate.delay).toBeUndefined();
});

test("broadcasts do not share an order", () => {
	const first = new Order();
	const second = new Order();
	const audio = new Estimator();
	const video = new Estimator();
	for (let i = 0; i <= 60; i++) {
		audio.write((200 + i * 20) * 1000, first, true);
		video.write(i * 20 * 1000, second, true);
	}
	expect(video.estimate.delay).toBeUndefined();
});
