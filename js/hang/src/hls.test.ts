import { expect, test } from "bun:test";
import type { Micro } from "@moq/clock";
import * as Json from "@moq/json";
import { Broadcast, Time } from "@moq/net";
import { u53 } from "./catalog";
import * as Legacy from "./container/legacy.ts";
import { Consumer, DEFAULT_DURATION, defaultName, type Record, Recorder, Segmenter } from "./hls.ts";

const us = (ms: number): Micro => (ms * 1000) as Micro;

const hls = { track: "video.hls", duration: u53(2000) };

function records() {
	const broadcast = new Broadcast.Producer();
	const track = broadcast.createTrack("video.hls", { timescale: Time.Timescale.MILLI });
	const producer = new Json.Window.Producer<Record>({ track });
	const consumer = Consumer.subscribe(broadcast.consume(), hls);
	const push = (value: Record) => producer.push({ value, at: Time.Timestamp.now() });
	const close = () => {
		producer.finish();
		consumer.close();
		broadcast.close();
	};
	return { producer, consumer, push, close };
}

test("the consumer yields records in microseconds, a stop included", async () => {
	const { producer, consumer, push, close } = records();
	push({ sequence: 0, group: 4, pts: 250 });
	push({ sequence: 1, group: 6, pts: 2250, duration: 0 });
	expect(await consumer.next()).toEqual({ push: { index: 0, entry: { sequence: 0, group: 4, pts: us(250) } } });
	expect(await consumer.next()).toEqual({
		push: { index: 1, entry: { sequence: 1, group: 6, pts: us(2250), duration: us(0) } },
	});
	producer.pop(1, Time.Timestamp.now());
	expect(await consumer.next()).toEqual({ pop: { start: 0, end: 1 } });
	close();
});

test("a record numbered unlike its window index is refused", async () => {
	const { consumer, push, close } = records();
	push({ sequence: 5, group: 0, pts: 0 });
	await expect(consumer.next()).rejects.toThrow("differs from its index");
	close();
});

test("a record without its group is refused", async () => {
	const { consumer, push, close } = records();
	push({ sequence: 0, pts: 0 } as Record);
	await expect(consumer.next()).rejects.toThrow();
	close();
});

// The segmenter cases mirror `rs/moq-mux/src/hls.rs`, so both cut the same records.

const hinted = { durationHint: us(4000) };

// Each record as `[group, pts, duration]` in wire units, checking the numbering.
function drain(segmenter: Segmenter, first = 0): [number, number, number | undefined][] {
	return segmenter.take().map((record, index) => {
		expect(record.sequence).toBe(first + index);
		return [record.group, record.pts, record.duration];
	});
}

// Groups `start..end`, one frame each, `step` ms apart.
function groups(segmenter: Segmenter, start: number, end: number, step: number) {
	for (let group = start; group < end; group++) segmenter.frame(group, us(group * step));
}

test("long groups get a segment each", () => {
	const segmenter = new Segmenter();
	groups(segmenter, 0, 3, 5000);
	expect(drain(segmenter)).toEqual([
		[0, 0, undefined],
		[1, 5000, undefined],
		[2, 10000, undefined],
	]);
	expect(segmenter.declared).toBe(us(5000));
});

test("short groups pack into a grid cell", () => {
	const segmenter = new Segmenter();
	groups(segmenter, 0, 10, 1000);
	expect(drain(segmenter)).toEqual([
		[0, 0, undefined],
		[4, 4000, undefined],
		[8, 8000, undefined],
	]);
});

test("a hint declares its smallest multiple at or past the duration", () => {
	expect(new Segmenter({ durationHint: us(1500) }).declared).toBe(us(4500));
	expect(new Segmenter(hinted).declared).toBe(us(4000));
});

test("a stopped first segment declares the grid", () => {
	const segmenter = new Segmenter();
	groups(segmenter, 0, 2, 1000);
	segmenter.close();
	expect(segmenter.declared).toBe(DEFAULT_DURATION);
	expect(drain(segmenter)).toEqual([
		[0, 0, undefined],
		[2, 1000, 0],
	]);
});

test("the end extends the stop", () => {
	const segmenter = new Segmenter(hinted);
	groups(segmenter, 0, 2, 1000);
	segmenter.end(us(2000));
	segmenter.close();
	expect(drain(segmenter)).toEqual([
		[0, 0, undefined],
		[2, 2000, 0],
	]);
	// Nothing reports after the close.
	segmenter.frame(5, us(9000));
	expect(drain(segmenter, 2)).toEqual([]);
});

test("a skipped sequence stops the segment", () => {
	const segmenter = new Segmenter(hinted);
	groups(segmenter, 0, 2, 1000);
	segmenter.end(us(1500));
	segmenter.frame(5, us(60000));
	expect(drain(segmenter)).toEqual([
		[0, 0, undefined],
		[2, 1500, 0],
		[5, 60000, undefined],
	]);
});

test("a stop ends the segment and the next group starts one", () => {
	const segmenter = new Segmenter(hinted);
	groups(segmenter, 0, 2, 1000);
	segmenter.end(us(2000));
	segmenter.stop(2);
	segmenter.frame(3, us(10000));
	expect(drain(segmenter)).toEqual([
		[0, 0, undefined],
		[2, 2000, 0],
		[3, 10000, undefined],
	]);
});

test("a report that goes backwards is refused", () => {
	const segmenter = new Segmenter(hinted);
	segmenter.frame(3, us(0));
	segmenter.frame(3, us(10));
	expect(() => segmenter.frame(2, us(20))).toThrow("group 2 after group 3");
});

test("an uneven GOP cuts on the grid", () => {
	const segmenter = new Segmenter();
	// 3s GOPs on a 4s grid: a group starts a segment only in a later cell than the one before.
	groups(segmenter, 0, 9, 3000);
	expect(drain(segmenter)).toEqual([
		[0, 0, undefined],
		[2, 6000, undefined],
		[3, 9000, undefined],
		[4, 12000, undefined],
		[6, 18000, undefined],
		[7, 21000, undefined],
		[8, 24000, undefined],
	]);
	expect(segmenter.declared).toBe(us(6000));
});

test("a sub-millisecond pts truncates on the wire", () => {
	// 120 frames of 30000/1001 fps video: a 4004.004 ms grid.
	const segmenter = new Segmenter({ durationHint: 33366.7 as Micro });
	expect(segmenter.declared).toBeCloseTo(4_004_004);
	groups(segmenter, 0, 1, 0);
	segmenter.frame(1, 4_004_999 as Micro);
	expect(drain(segmenter)).toEqual([
		[0, 0, undefined],
		[1, 4004, undefined],
	]);
});

test("a recorder advertises its declaration rounded up to whole milliseconds", () => {
	const broadcast = new Broadcast.Producer();
	const recorder = new Recorder(broadcast.createTrack(defaultName("video"), Recorder.INFO), {
		durationHint: 33366.7 as Micro,
	});
	expect(recorder.hls.peek()).toEqual({ track: "video.hls", duration: u53(4005) });
	recorder.finish();
	broadcast.close();
});

// A legacy producer reports each group it writes, and a recorder publishes and advertises them.
test("a recorded legacy track publishes its records and declares its duration", async () => {
	const broadcast = new Broadcast.Producer();
	const media = broadcast.createTrack("video", { timescale: Time.Timescale.MICRO });
	const recorder = new Recorder(broadcast.createTrack(defaultName("video"), Recorder.INFO));
	expect(recorder.hls.peek()).toBeUndefined();

	const producer = new Legacy.Producer(media, new Legacy.Format("video"), recorder);
	const payload = new Uint8Array([1]);
	// 2s GOPs of 30 fps-ish frames, then a pause and two more GOPs.
	for (let ms = 0; ms < 6000; ms += 100) producer.encode(payload, us(ms), ms % 2000 === 0);
	producer.discontinuity(us(6000));
	for (let ms = 20000; ms < 24000; ms += 100) producer.encode(payload, us(ms), ms % 2000 === 0);
	producer.close();
	expect(recorder.hls.peek()).toEqual({ track: "video.hls", duration: u53(4000) });

	const consumer = Consumer.subscribe(broadcast.consume(), { track: "video.hls", duration: u53(4000) });
	const entries = [];
	for await (const event of consumer) if ("push" in event) entries.push(event.push.entry);
	expect(entries).toEqual([
		{ sequence: 0, group: 0, pts: us(0) },
		{ sequence: 1, group: 2, pts: us(4000) },
		{ sequence: 2, group: 3, pts: us(6000), duration: us(0) },
		{ sequence: 3, group: 4, pts: us(20000) },
		{ sequence: 4, group: 6, pts: us(24000), duration: us(0) },
	]);
	broadcast.close();
});

test("a report out of order aborts the records but not the media", async () => {
	const broadcast = new Broadcast.Producer();
	const records = broadcast.createTrack(defaultName("video"), Recorder.INFO);
	const recorder = new Recorder(records);
	recorder.frame(3, us(0));
	recorder.frame(2, us(10));
	expect(records.closed.peek()).toBeInstanceOf(Error);
	// Later reports are ignored.
	recorder.frame(4, us(20));
	recorder.finish();
	broadcast.close();
});
