import { expect, test } from "bun:test";
import * as Json from "@moq/json";
import { Broadcast, type Time } from "@moq/net";
import { u53 } from "./catalog";
import { Consumer, type Record } from "./timeline.ts";

const us = (ms: number): Time.Micro => (ms * 1000) as Time.Micro;

const archive = (timescale: number) => ({ timelines: { video: "video.timeline.z" }, timescale: u53(timescale) });

test("timeline consumer yields converted push and pop events", async () => {
	const broadcast = new Broadcast.Producer();
	const track = broadcast.createTrack("video.timeline.z");
	const producer = new Json.Window.Producer<Record>({ track, compression: true });
	const consumer = Consumer.subscribe(broadcast.consume(), archive(1000), "video");
	producer.push({ sequence: 0, pts: 250, duration: 1250, start: { group: 1 }, end: { group: 2, frame: 3 } });
	expect(await consumer.next()).toEqual({
		push: {
			index: 0,
			entry: { sequence: 0, pts: us(250), duration: us(1250), start: { group: 1 }, end: { group: 2, frame: 3 } },
		},
	});
	producer.pop(1);
	expect(await consumer.next()).toEqual({ pop: { start: 0, end: 1 } });
	producer.finish();
	consumer.close();
	broadcast.close();
});

test("timeline consumer floors fractional microseconds without floating point drift", async () => {
	const broadcast = new Broadcast.Producer();
	const track = broadcast.createTrack("video.timeline.z");
	const producer = new Json.Window.Producer<Record>({ track, compression: true });
	const consumer = Consumer.subscribe(broadcast.consume(), archive(3), "video");
	producer.push({ sequence: 0, pts: 1, duration: 2, start: { group: 0 }, end: { group: 1 } });
	expect(await consumer.next()).toEqual({
		push: {
			index: 0,
			entry: {
				sequence: 0,
				pts: 333_333 as Time.Micro,
				duration: 666_666 as Time.Micro,
				start: { group: 0 },
				end: { group: 1 },
			},
		},
	});
	producer.finish();
	consumer.close();
	broadcast.close();
});

test("a record numbered unlike its window index is refused", async () => {
	const broadcast = new Broadcast.Producer();
	const track = broadcast.createTrack("video.timeline.z");
	const producer = new Json.Window.Producer<Record>({ track, compression: true });
	const consumer = Consumer.subscribe(broadcast.consume(), archive(1000), "video");
	producer.push({ sequence: 5, pts: 0, duration: 1000, start: { group: 0 }, end: { group: 1 } });
	await expect(consumer.next()).rejects.toThrow();
	producer.finish();
	consumer.close();
	broadcast.close();
});

test("a track without a timeline is refused", () => {
	const broadcast = new Broadcast.Producer();
	expect(() => Consumer.subscribe(broadcast.consume(), archive(1000), "audio")).toThrow();
	broadcast.close();
});
