import { expect, test } from "bun:test";
import type { Time } from "@moq/net";
import { consumeFrame } from "./decoder.ts";

class StubFrame {
	readonly timestamp: number;
	closed = false;
	constructor(timestamp: number) {
		this.timestamp = timestamp;
	}
	close(): void {
		this.closed = true;
	}
}

const us = (ms: number) => (ms * 1_000) as Time.Micro;
const ms = (n: number) => n as Time.Milli;

test("returns undefined when the queue is empty", () => {
	const queue: StubFrame[] = [];
	expect(consumeFrame(queue, ms(100))).toBeUndefined();
	expect(queue.length).toBe(0);
});

test("returns undefined when every frame is still in the future", () => {
	const a = new StubFrame(us(50));
	const b = new StubFrame(us(100));
	const queue = [a, b];
	expect(consumeFrame(queue, ms(10))).toBeUndefined();
	expect(queue).toEqual([a, b]);
	expect(a.closed).toBe(false);
	expect(b.closed).toBe(false);
});

test("returns the frame when its PTS equals now", () => {
	const a = new StubFrame(us(33));
	const queue = [a];
	expect(consumeFrame(queue, ms(33))).toBe(a);
	expect(queue.length).toBe(0);
	expect(a.closed).toBe(false);
});

test("returns the only frame when it is ready", () => {
	const a = new StubFrame(us(0));
	const queue = [a];
	expect(consumeFrame(queue, ms(16))).toBe(a);
	expect(queue.length).toBe(0);
	expect(a.closed).toBe(false);
});

test("returns the newest ready frame and closes older ones", () => {
	const a = new StubFrame(us(0));
	const b = new StubFrame(us(33));
	const c = new StubFrame(us(66));
	const queue = [a, b, c];

	expect(consumeFrame(queue, ms(50))).toBe(b);
	expect(queue).toEqual([c]);
	expect(a.closed).toBe(true);
	expect(b.closed).toBe(false);
	expect(c.closed).toBe(false);
});

test("preserves future frames when there are no ready ones", () => {
	const a = new StubFrame(us(100));
	const b = new StubFrame(us(200));
	const queue = [a, b];

	expect(consumeFrame(queue, ms(50))).toBeUndefined();
	expect(queue).toEqual([a, b]);
	expect(a.closed).toBe(false);
	expect(b.closed).toBe(false);
});

test("60Hz vsync with 120fps content drops the older frame each tick", () => {
	const older = new StubFrame(us(8));
	const newer = new StubFrame(us(16));
	const queue = [older, newer];

	expect(consumeFrame(queue, ms(16))).toBe(newer);
	expect(queue.length).toBe(0);
	expect(older.closed).toBe(true);
	expect(newer.closed).toBe(false);
});

test("120Hz vsync with 30fps content returns undefined between frames", () => {
	const frame = new StubFrame(us(0));
	const queue = [frame];

	expect(consumeFrame(queue, ms(0))).toBe(frame);
	expect(queue.length).toBe(0);

	expect(consumeFrame(queue, ms(8))).toBeUndefined();
	expect(consumeFrame(queue, ms(16))).toBeUndefined();
	expect(consumeFrame(queue, ms(24))).toBeUndefined();
});

test("returns a late frame when nothing newer is queued", () => {
	const late = new StubFrame(us(10));
	const queue = [late];

	expect(consumeFrame(queue, ms(500))).toBe(late);
	expect(queue.length).toBe(0);
	expect(late.closed).toBe(false);
});

test("closes every older frame, not just the immediately preceding one", () => {
	const frames = [0, 8, 16, 24, 32, 40].map((t) => new StubFrame(us(t)));
	const queue = [...frames];

	expect(consumeFrame(queue, ms(40))).toBe(frames[5]);
	expect(queue.length).toBe(0);
	for (let i = 0; i < 5; i++) {
		expect(frames[i].closed).toBe(true);
	}
	expect(frames[5].closed).toBe(false);
});

test("60fps content on 60Hz vsync paints one frame per tick with no growth", () => {
	const queue: StubFrame[] = [];
	const created: StubFrame[] = [];
	const painted: number[] = [];

	const VSYNC_MS = 1000 / 60;
	const FRAME_MS = 1000 / 60;

	for (let i = 0; i < 30; i++) {
		const frame = new StubFrame(us(i * FRAME_MS));
		queue.push(frame);
		created.push(frame);

		const picked = consumeFrame(queue, (i * VSYNC_MS) as Time.Milli);
		if (picked) painted.push(picked.timestamp);
	}

	expect(painted.length).toBe(30);
	expect(created.filter((f) => f.closed).length).toBe(0);
	expect(queue.length).toBe(0);
});
